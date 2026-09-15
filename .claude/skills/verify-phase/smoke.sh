#!/usr/bin/env bash
# Smoke test of `printhub serve` against the fake printer. Usage: smoke.sh [extra-checks.sh]
set -u

cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)" || exit 1
cargo build -q -p printhub -p fakeprinter || exit 1

D=$(mktemp -d)
PORT=18089
B=http://127.0.0.1:$PORT
J=$D/jar
failed=0

./target/debug/fakeprinter >"$D/fp.env" 2>"$D/fp.log" &
FP=$!
for _ in {1..50}; do grep -q PRINTER_CAMERA_PORT "$D/fp.env" && break; sleep 0.1; done
set -a; . "$D/fp.env"; set +a

DATA_DIR=$D/data LISTEN_ADDR=127.0.0.1:$PORT ADMIN_USERNAME=admin ADMIN_PASSWORD=smoke-password-1 \
  ./target/debug/printhub serve >"$D/serve.log" 2>&1 &
SV=$!

check() { # check <label> <expected> <actual>
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: expected $2, got $3"; failed=1; fi
}

check healthz ok "$(curl -s --retry-connrefused --retry 20 --retry-delay 1 $B/healthz)"
check login 303 "$(curl -s -o /dev/null -w '%{http_code}' -c "$J" -H "Origin: $B" \
  -d 'username=admin&password=smoke-password-1' $B/login)"

# Registration with the fake printer takes a moment after startup.
for _ in {1..30}; do
  card=$(curl -s -b "$J" $B/)
  grep -q Connected <<<"$card" && grep -q Idle <<<"$card" && break
  sleep 0.2
done
check "dashboard connected" yes "$(grep -q Connected <<<"$card" && echo yes)"

if [ $# -ge 1 ]; then
  echo "--- extra checks: $1"
  . "$1"
fi

kill -TERM $SV; wait $SV; check "serve exits cleanly on SIGTERM" 0 "$?"
kill $FP; wait $FP 2>/dev/null

echo "--- serve log"
sed -E 's/\x1b\[[0-9;]*m//g' "$D/serve.log"
rm -rf "$D"
exit $failed
