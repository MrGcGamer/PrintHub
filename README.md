# PrintHub

A shared print queue for one Elegoo Centauri Carbon 2, for a group of friends who print on it.
It answers the questions a shared printer creates: whose turn is it, whose filament went into
that, and who owes whom.

One Docker image, configured by environment variables, serving plain HTTP on port 8080.

- **Queue.** Upload an STL and PrintHub slices it with the bundled OrcaSlicer and Elegoo's
  profiles, or upload G-code you sliced yourself. A job starts only when the printer is idle,
  the bed is confirmed clear, the mounted nozzle matches and the schedule allows.
- **Dashboard.** Live printer state, temperatures, trays and the camera, over SSE. Pause,
  resume and stop; the chamber light is available to everyone.
- **Filament.** Spools bound to the printer's CANVAS trays. Prints deduct grams from the spool
  they used, and weigh-ins correct the drift.
- **Statistics.** Who printed how much, with whose filament, and what that leaves people owing
  each other. Balances are over all time, net of recorded payments.
- **Accounts.** Invite links, per-permission grants, and admins who hold everything.
- **Help.** A searchable wiki built into the app, linked from wherever a setting needs it.

## Run

Edit `compose.yaml`: at least `PRINTER_HOST`, `PRINTER_ACCESS_CODE` and the admin account. Then:

```sh
docker compose build
docker compose run --rm printhub slice-selftest
docker compose run --rm printhub probe
docker compose up -d
```

- `slice-selftest` slices a 20 mm cube with the bundled profiles and prints the grams used. It
  needs no printer, and is the quickest way to see whether a slice fits in a small machine's RAM.
- `probe` checks every printer endpoint PrintHub uses and prints a report. It changes nothing on
  the printer.
- On first start with no admin, `ADMIN_USERNAME` and `ADMIN_PASSWORD` create one. Remove them
  from `compose.yaml` afterwards; everyone else joins through invite links.
- The printer does not report its nozzle, so it is recorded on the dashboard. Admins always
  can; under Users, an admin grants "Record the mounted nozzle" to whoever has the printer at
  their place. STL uploads are sliced for the recorded nozzle, and a job sliced for another
  nozzle waits in the queue.

> The container runs as UID 10001 with a read-only root filesystem. `/data` must be writable by
> that UID; a named volume is, a bind mount has to be `chown`ed first.

Behind a reverse proxy that terminates HTTPS, such as `tailscale serve`, set `TRUST_PROXY=true`
so the session cookie is marked `Secure`, and origin checks and invite links use
`X-Forwarded-Proto` and `X-Forwarded-Host`.

The printer must be in LAN Only Mode, and PrintHub is the only thing that talks to it besides
its own screen — it allows few simultaneous clients.

## Configuration

| Variable | Default | |
|---|---|---|
| `PRINTER_HOST` | required | Printer hostname or IP address |
| `PRINTER_ACCESS_CODE` | `123456` | As set on the printer's touchscreen |
| `PRINTER_SN` | discovered | Serial number; set it if UDP discovery does not reach the printer |
| `PRINTER_NOZZLE` | `0.4` | `0.2`, `0.4`, `0.6` or `0.8`; assumed mounted until someone records the nozzle on the dashboard |
| `PRINTER_MQTT_PORT` | `1883` | |
| `PRINTER_UPLOAD_PORT` | `80` | |
| `PRINTER_CAMERA_PORT` | `8080` | |
| `LISTEN_ADDR` | `0.0.0.0:8080` | The address PrintHub itself serves on |
| `DATA_DIR` | `/data` | Database, uploads and sliced files |
| `TZ` | the system's zone | IANA zone the print schedule and every shown time are in |
| `ADMIN_USERNAME`, `ADMIN_PASSWORD` | unset | Creates the first admin; ignored once one exists |
| `TRUST_PROXY` | `false` | Honour `X-Forwarded-Proto` and `X-Forwarded-Host` |
| `CAMERA_ENABLED` | `true` | |
| `MAX_UPLOAD_MB` | `200` | |
| `SLICE_TIMEOUT` | `15m` | `90s`, `15m`, `1h` |
| `ESTIMATE_MARGIN` | `1.15` | Multiplies the print-time estimate when checking a job ends before a schedule window closes |
| `ORCA_SLICER` | `/opt/orcaslicer/bin/orca-slicer` | Without it, only G-code uploads are accepted |
| `ORCA_PROFILES` | `/opt/orcaslicer/resources/profiles/Elegoo` | |
| `RUST_LOG` | `info` for `serve`, else `warn` | Log filter, e.g. `debug` or `printhub=debug` |

## Limits

- One printer, and the multi-material CANVAS it came with.
- A model can be scaled down on upload but not up: OrcaSlicer's CLI segfaults on any factor
  above 1 ([#13328](https://github.com/OrcaSlicer/OrcaSlicer/issues/13328), fixed upstream but
  not yet in a release). Enlarge in your own slicer and upload the G-code.
- Nothing checks that a model fits the bed before slicing it.

## Development

Rust 1.96 and `sqlx-cli`. `sqlx::query!` checks queries against `DATABASE_URL` at compile time:

```sh
cargo install sqlx-cli --no-default-features --features sqlite
echo "DATABASE_URL=sqlite://$PWD/target/dev.db" > .env
set -a && . ./.env && set +a
sqlx database create
sqlx migrate run --source crates/printhub/migrations
```

- After changing a query, run `cargo sqlx prepare --workspace -- --all-targets` and commit
  `.sqlx/`; the Docker build compiles from that offline data and fails when it is stale.
- `cargo run -p fakeprinter` starts an emulated printer (MQTT, upload, camera) and prints the
  `PRINTER_*` variables that point PrintHub at it. Then `cargo run -p printhub -- serve`.
- The test that runs the real slicer is skipped unless `ORCA_SLICER` and `ORCA_PROFILES` are set.
- `.claude/skills/verify-phase` holds the full check chain and a smoke test of `serve` against
  the fake printer.
- The icons in `crates/printhub/static` are rendered from `logo.svg` and `logo-maskable.svg`
  (full-bleed, for launchers that crop their own shape). After editing either, run
  `crates/printhub/static/render-icons.sh` (needs Inkscape and ImageMagick).

## Build for another architecture

The image builds for `linux/arm64` and `linux/amd64`. Rust is cross-compiled on the build host,
so only the final `apt-get` step runs emulated.

```sh
docker buildx build --platform linux/arm64,linux/amd64 -t <registry>/printhub:latest --push .
```
