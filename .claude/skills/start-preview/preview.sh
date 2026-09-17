#!/usr/bin/env bash
# Runs the built arm64 image on http://localhost:8080 against the fake printer, keeping the
# printhub-preview-data volume. Usage: preview.sh [stop]
set -u

cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)" || exit 1
B=http://127.0.0.1:8080

if [ "${1:-}" = stop ]; then
  docker rm -f printhub-preview printhub-preview-printer >/dev/null 2>&1
  echo "preview stopped; data kept in printhub-preview-data"
  exit 0
fi

# Colima can report Running while its VM is dead, and the socket then refuses connections.
if ! docker info >/dev/null 2>&1; then
  echo "docker unreachable; restarting colima"
  colima stop --force >/dev/null 2>&1
  colima start >/dev/null 2>&1 || { echo "colima start failed"; exit 1; }
fi

docker image inspect printhub:arm64 >/dev/null 2>&1 || {
  echo "no printhub:arm64 image: docker buildx build --platform linux/arm64 -t printhub:arm64 --load ."
  exit 1
}

if ! docker run --rm --entrypoint test -v printhub-smoke-bin:/b printhub:arm64 -x /b/fakeprinter 2>/dev/null; then
  echo "building a Linux fakeprinter into printhub-smoke-bin"
  docker image inspect printhub-build:arm64 >/dev/null 2>&1 ||
    docker buildx build --platform linux/arm64 --target build -t printhub-build:arm64 --load . >/dev/null || exit 1
  docker volume create printhub-smoke-bin >/dev/null
  docker run --rm -v printhub-smoke-bin:/out -w /src printhub-build:arm64 sh -c \
    'SQLX_OFFLINE=true cargo zigbuild --release --locked -p fakeprinter --target aarch64-unknown-linux-musl >/dev/null && cp target/aarch64-unknown-linux-musl/release/fakeprinter /out/' || exit 1
fi

# Recreated as a pair every time: the fake printer takes new random ports on each start, so a
# restarted serve container would keep pointing at the old ones.
docker rm -f printhub-preview printhub-preview-printer >/dev/null 2>&1
env_file=$(mktemp)
docker run -d --name printhub-preview-printer --restart unless-stopped --no-healthcheck \
  -p 127.0.0.1:8080:8080 -v printhub-smoke-bin:/fake:ro --entrypoint /fake/fakeprinter \
  printhub:arm64 >/dev/null || exit 1
for _ in {1..50}; do
  docker logs printhub-preview-printer 2>/dev/null | grep -q PRINTER_CAMERA_PORT && break
  sleep 0.1
done
docker logs printhub-preview-printer 2>/dev/null | grep '^PRINTER_' >"$env_file"
docker run -d --name printhub-preview --restart unless-stopped \
  --network container:printhub-preview-printer --env-file "$env_file" \
  -e ADMIN_USERNAME=admin -e ADMIN_PASSWORD=printhub-preview-1 -e TZ=Europe/Berlin \
  -v printhub-preview-data:/data \
  --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
  printhub:arm64 >/dev/null || exit 1
rm -f "$env_file"

# Colima forwards the published port some seconds after start, answering empty until then.
for _ in {1..60}; do
  [ "$(curl -s $B/healthz)" = ok ] && break
  sleep 1
done
if [ "$(curl -s $B/healthz)" != ok ]; then
  echo "preview did not come up"
  docker logs printhub-preview 2>&1 | tail -20
  exit 1
fi
echo "preview running at http://localhost:8080 (admin / printhub-preview-1)"
docker logs printhub-preview 2>&1 | sed -E 's/\x1b\[[0-9;]*m//g' | grep -E 'WARN|ERROR'
exit 0
