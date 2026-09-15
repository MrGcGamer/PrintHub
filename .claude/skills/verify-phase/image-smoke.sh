#!/usr/bin/env bash
# Smoke test of the built image: `serve` in a container with compose's hardening, against the
# fake printer, through an STL upload sliced inside the container.
# Usage: image-smoke.sh <image> <fakeprinter-volume>
# The volume must hold a Linux `fakeprinter` binary for the image's architecture.
set -u

IMAGE=${1:?image}
FP_VOLUME=${2:?volume holding a Linux fakeprinter binary}
PORT=18090
B=http://127.0.0.1:$PORT
D=$(mktemp -d)
J=$D/jar
failed=0

check() { # check <label> <expected> <actual>
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: expected $2, got $3"; failed=1; fi
}

remove_containers() {
  docker rm -f printhub-smoke-serve printhub-smoke-printer >/dev/null 2>&1
  docker volume rm printhub-smoke-data >/dev/null 2>&1
}
remove_containers
docker volume create printhub-smoke-data >/dev/null

# The fake printer listens on 127.0.0.1 only, so `serve` joins its network namespace, and the
# web port is published on the printer's container.
docker run -d --name printhub-smoke-printer -p 127.0.0.1:$PORT:8080 \
  -v "$FP_VOLUME":/fake:ro --entrypoint /fake/fakeprinter "$IMAGE" >/dev/null || exit 1
for _ in {1..50}; do
  docker logs printhub-smoke-printer 2>/dev/null | grep -q PRINTER_CAMERA_PORT && break
  sleep 0.1
done
docker logs printhub-smoke-printer 2>/dev/null | grep '^PRINTER_' >"$D/fp.env"

docker run -d --name printhub-smoke-serve --network container:printhub-smoke-printer \
  --env-file "$D/fp.env" -e ADMIN_USERNAME=admin -e ADMIN_PASSWORD=smoke-password-1 \
  -e TZ=Europe/Berlin -v printhub-smoke-data:/data \
  --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
  "$IMAGE" >/dev/null || exit 1

# Colima forwards a published port some seconds after the container starts, answering with
# empty replies until then, which curl's own retries do not cover.
for _ in {1..60}; do
  healthz=$(curl -s $B/healthz)
  [ "$healthz" = ok ] && break
  sleep 1
done
check healthz ok "$healthz"
check login 303 "$(curl -s -o /dev/null -w '%{http_code}' -c "$J" -H "Origin: $B" \
  -d 'username=admin&password=smoke-password-1' $B/login)"

for _ in {1..30}; do
  card=$(curl -s -b "$J" $B/)
  grep -q Connected <<<"$card" && grep -q Idle <<<"$card" && break
  sleep 0.2
done
check "dashboard connected" yes "$(grep -q Connected <<<"$card" && echo yes)"

check "create spool" 303 "$(curl -s -o /dev/null -w '%{http_code}' -b "$J" -H "Origin: $B" \
  --data-urlencode material=PLA --data-urlencode brand=Elegoo --data-urlencode color_name=Blue \
  --data-urlencode 'color_hex=#2850DF' --data-urlencode initial_grams=1000 \
  --data-urlencode owner_id= $B/spools)"

python3 - "$D/cube.stl" <<'EOF'
import sys
s = 20.0
v = lambda i: tuple(s if i >> b & 1 else 0.0 for b in (2, 1, 0))
faces = [(0,1,3),(0,3,2),(4,6,7),(4,7,5),(0,4,5),(0,5,1),(2,3,7),(2,7,6),(0,2,6),(0,6,4),(1,5,7),(1,7,3)]
out = ["solid cube"]
for f in faces:
    out += [" facet normal 0 0 0", "  outer loop"]
    out += ["   vertex %g %g %g" % v(i) for i in f]
    out += ["  endloop", " endfacet"]
open(sys.argv[1], "w").write("\n".join(out + ["endsolid cube", ""]))
EOF
location=$(curl -s -o /dev/null -w '%{redirect_url}' -b "$J" -H "Origin: $B" \
  -F file=@"$D/cube.stl" -F spool_id=1 -F 'process=0.20mm Standard @Elegoo CC2 0.4 nozzle' \
  -F infill=15 $B/jobs)
check "upload STL" "$B/jobs/1" "$location"

for _ in {1..120}; do
  job=$(curl -s -b "$J" $B/jobs/1)
  grep -q -E 'Waiting for confirmation|Failed' <<<"$job" && break
  sleep 1
done
check "sliced in the container" yes "$(grep -q 'Waiting for confirmation' <<<"$job" && echo yes)"

for _ in {1..60}; do
  health=$(docker inspect -f '{{.State.Health.Status}}' printhub-smoke-serve)
  [ "$health" = starting ] || break
  sleep 1
done
check "docker healthcheck" healthy "$health"

docker stop -t 15 printhub-smoke-serve >/dev/null
check "serve exits cleanly on SIGTERM" 0 "$(docker inspect -f '{{.State.ExitCode}}' printhub-smoke-serve)"

echo "--- serve log"
docker logs printhub-smoke-serve 2>&1 | sed -E 's/\x1b\[[0-9;]*m//g'
remove_containers
rm -rf "$D"
exit $failed
