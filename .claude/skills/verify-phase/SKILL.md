---
name: verify-phase
description: Verify a PrintHub change before calling it done or committing — fmt, clippy, sqlx offline data, tests, offline build, and a real-process smoke test of `printhub serve` against the fake printer. Use at the end of every phase or any change to queries, migrations, routes or startup.
---

# Verify a PrintHub phase

Run from the workspace root (`HomeLab/PrintHub`). Commit only after every step passes, one
commit per phase, and leave `STATUS.md` untracked.

## 1. Schema and query data

New migration? Apply it to the dev database first, or `sqlx::query!` will not compile:

```sh
set -a && . ./.env && set +a
sqlx migrate run --source crates/printhub/migrations
```

After adding or changing any `query!`/`query_as!`, regenerate the offline data (it also
deletes stale entries) and commit `.sqlx/` with the change:

```sh
cargo sqlx prepare --workspace -- --all-targets
```

## 2. The check chain

These share the cargo target lock, so run them in sequence, each with its own exit code.
The shell is zsh: there is no `PIPESTATUS`, and `cmd | grep` reports grep's status, so send
output to a log and read `$?` directly.

```sh
set -a && . ./.env && set +a
cargo fmt --check; echo "fmt=$?"
cargo clippy --workspace --all-targets -- -D warnings >/tmp/clippy.log 2>&1; echo "clippy=$?"
cargo sqlx prepare --workspace --check -- --all-targets >/tmp/prep.log 2>&1; echo "prepare=$?"
cargo test --workspace >/tmp/test.log 2>&1; echo "test=$?"; grep -E '^test result|FAILED|panicked' /tmp/test.log
env -u DATABASE_URL SQLX_OFFLINE=true cargo build -p printhub >/tmp/offline.log 2>&1; echo "offline=$?"
```

- `fmt --check` failing: run `cargo fmt` and re-run the chain. It rewrites files, so re-read
  any file before editing it again.
- The offline build is what the Docker image does. It fails when `.sqlx/` is stale even if
  everything else passes with `DATABASE_URL` set.

## 3. Smoke test the real process

The web tests drive the router in-process. This step proves `serve` itself — file database
migrations, admin bootstrap, background tasks, graceful shutdown:

```sh
.claude/skills/verify-phase/smoke.sh [extra-checks.sh]
```

It starts `fakeprinter`, runs `printhub serve` on a fresh `DATA_DIR`, logs in as a
bootstrapped admin, checks the dashboard reaches `Connected` and `Idle`, sends SIGTERM and
expects exit 0. An optional extra-checks file is sourced after login with `$B` (base URL)
and `$J` (cookie jar) set; exercise the phase's new routes there with `curl -b $J -H
"Origin: $B"`. POSTs without that `Origin` header are refused with 403.

Read the printed serve log: a warning or error there is a failure even when every status
code looked right.

## 4. The Docker image

Needed when the `Dockerfile`, `compose.yaml`, startup, or anything the slicer depends on
changed. This Mac runs Docker through Colima (see `CLEANUP.md`); start it with `colima start`.

```sh
docker buildx build --platform linux/arm64 -t printhub:arm64 --load . >/tmp/image.log 2>&1; echo "image=$?"
docker buildx build --platform linux/arm64 --target build -t printhub-build:arm64 --load . >/dev/null 2>&1
docker compose config >/dev/null; echo "compose=$?"
docker run --rm --read-only --tmpfs /tmp --cap-drop ALL printhub:arm64 slice-selftest; echo "selftest=$?"
```

`image-smoke.sh` runs `serve` in the image with compose's hardening against the fake printer,
uploads an STL and waits for it to be sliced in the container, then checks the Docker
healthcheck and a clean stop. It needs a Linux `fakeprinter` binary in a volume:

```sh
docker volume create printhub-smoke-bin
docker run --rm -v printhub-smoke-bin:/out -w /src printhub-build:arm64 sh -c \
  'SQLX_OFFLINE=true cargo zigbuild --release --locked -p fakeprinter --target aarch64-unknown-linux-musl && cp target/aarch64-unknown-linux-musl/release/fakeprinter /out/'
.claude/skills/verify-phase/image-smoke.sh printhub:arm64 printhub-smoke-bin
```

## 5. Report

The fake printer models the documented protocol, not the real firmware. Say that nothing is
verified against the real CC2, and add anything the phase depends on that only the printer
(or the Pi5) can settle to `STATUS.md` under "Still unverified".
