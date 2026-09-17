# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A shared print queue for one Elegoo Centauri Carbon 2 (CC2): accounts for a group of friends,
a live dashboard and camera, a filament inventory bound to the printer's trays, a queue that
slices STL uploads with the OrcaSlicer CLI and starts jobs when the printer, bed, nozzle and
schedule allow, and statistics on who printed with whose filament. Rust (axum, askama + htmx,
sqlx/SQLite, rumqttc), shipped as one Docker image configured by environment variables (see the
table in `README.md`).

**Nothing has run against the real printer yet.** Everything is verified against
`crates/fakeprinter`, which models the documented protocol, not the firmware. The open
questions that only the printer can answer are listed in `STATUS.md` under "Still unverified".

The deployment host (a Raspberry Pi 5) and every other HomeLab server are operated by the
user. You cannot reach them: give the exact commands to run and ask for the output.

`STATUS.md` (untracked) is the handoff between sessions: what is done, in progress, and
unverified. Read it first when resuming, update it at the end. `CLEANUP.md` (untracked) lists
everything installed on this Mac for the project; add to it whenever you install something.

## Commands

Every `cargo` command that compiles queries needs `DATABASE_URL` from the git-ignored `.env`:

```sh
set -a && . ./.env && set +a
```

- One-time dev database: `sqlx database create && sqlx migrate run --source crates/printhub/migrations`
- New migration: apply it with `sqlx migrate run --source crates/printhub/migrations` before
  building, or `query!` will not compile.
- After adding or changing any `query!`/`query_as!`: `cargo sqlx prepare --workspace -- --all-targets`
  and commit `.sqlx/`. The Docker and CI builds compile offline from it.
- Single test: `cargo test -p printhub --test web <name>` (integration),
  `cargo test -p printhub --lib <module>::tests::<name>` (unit).
- The real-slicer test is skipped unless `ORCA_SLICER` and `ORCA_PROFILES` are set.
- Run against the emulator: `cargo run -p fakeprinter` prints the `PRINTER_*` variables; export
  them, then `cargo run -p printhub -- serve` (or `probe`, `slice-selftest`).

Verification is a skill, not ad hoc: `.claude/skills/verify-phase` holds the full chain (fmt,
clippy with `-D warnings`, sqlx prepare check, tests, offline build), `smoke.sh` for the real
`serve` process, and the Docker image checks including `image-smoke.sh`. Run it before calling
a change done or committing. `.claude/skills/start-preview` runs the built image on
localhost:8080.

The shell is zsh: there is no `PIPESTATUS`, and `cmd | grep` reports grep's status, so write
output to a log and read `$?` directly.

## Architecture

`crates/printhub` is a library plus a thin `main.rs` (clap: `serve`, `probe`, `healthcheck`,
`slice-selftest`). `crates/fakeprinter` is a rumqttd-based CC2 emulator (MQTT, HTTP upload,
MJPEG camera) used by the integration tests and for local runs.

**Printer link.** `cc2::PrinterClient` owns the MQTT session: registration, heartbeat,
request/response matched by id, and a status cache built from deltas that re-fetches the full
status when `result.sequence` has a gap. `printer::PrinterLink` keeps retrying discovery while
the printer is off and publishes a `PrinterSnapshot` on a watch channel. Uploads are plain HTTP
(`cc2::upload`); the camera is one upstream MJPEG connection fanned out by `camera::CameraHub`.

**Shared state.** `web::AppState` holds the database, config, printer link, camera hub,
optional slicer and uploader, plus watch channels the UI and dispatcher read without queries:
`bindings` (tray ↔ spool) and `nozzle` (the recorded mounted nozzle). Whoever changes the
underlying rows must call `refresh_bindings()` / `refresh_nozzle()`. `queue_changed` wakes the
dispatcher.

**Jobs.** `jobs::transition` is the only way a job changes state, and it applies only if the
job is still in the expected state, so racing tasks cannot both win. Whether a job may start is
a pure function, `jobs::check_start` → a `slot_map` or the first `BlockReason` (printer offline,
another job active, printer busy, nozzle mismatch, bed not clear, spools, schedule, in that
order), fed by `dispatcher::Readiness`, which
the job pages reuse to show why a job waits. `dispatcher` uploads as `printhub-<id>.gcode`,
starts it with the `slot_map`, and follows the print by that filename and its sub-status.
`remaining_grams` on a spool only changes together with a `consumption` row, in one
transaction.

**Statistics.** `stats` reads the `consumption` ledger and finished jobs, and aggregates them in
pure functions. Each ledger row records the spool's owner and the filament's value when it is
written, so editing a spool later does not rewrite who used whose filament or what is owed.
Balances are always over all time, net of `settlements`; only the recipient or an admin records
a payment. Chart colours (`web/chart.rs`, `--series-*` in `app.css`) are assigned by account id,
so a person keeps their colour across periods.

**Slicing.** The OrcaSlicer CLI ignores `inherits`, so `slicer::ProfileLibrary` flattens every
profile and writes it back marked `from: system` (the CLI only treats a process as compatible
with a machine for system profiles). The machine profile is chosen per slice from the nozzle
(`slicer::machine_name`); one slice runs at a time. Hard-won CLI facts are recorded in
`STATUS.md` under Phase 4.

**Web.** Server-rendered askama templates with htmx. The printer card is re-rendered over SSE
whenever a watch channel changes; forms must sit outside the SSE-swapped element or updates
reset them. Every non-GET request must pass the same-origin guard (tests send an `Origin`
header). Page outcomes are passed as fixed codes (`?done=`, `?problem=`) mapped to text, never
as free text in the URL.

**Accounts.** Admins hold every `accounts::Permission` implicitly; members get them by grant on
the users page. An admin can never be made a member, and the last enabled admin cannot be
disabled. `accounts` and `jobs` functions take timestamps instead of reading the clock.

**Packaging.** The `Dockerfile` cross-compiles with cargo-zigbuild on the build platform,
unpacks the OrcaSlicer AppImage (checksum-pinned) without executing it, and runs as UID 10001
on a read-only root with a tmpfs `/tmp` (the slicer needs it). `.github/workflows/image.yml`
builds amd64 and arm64 images to GHCR.
