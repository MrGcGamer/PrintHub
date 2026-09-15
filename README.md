# PrintHub

A shared print queue for one Elegoo Centauri Carbon 2: accounts for a group of friends, a live
printer dashboard and camera, a filament inventory bound to the printer's trays, and a queue
that slices STL uploads with OrcaSlicer and starts jobs when the bed is clear and the schedule
allows.

One Docker image, configured by environment variables, serving plain HTTP on port 8080.

## Run

Edit `compose.yaml`: at least `PRINTER_HOST`, `PRINTER_ACCESS_CODE` and the admin account. Then:

```sh
docker compose build
docker compose run --rm printhub slice-selftest
docker compose run --rm printhub probe
docker compose up -d
```

- `slice-selftest` slices a 20 mm cube with the bundled profiles and prints the grams used. It
  needs no printer.
- `probe` checks every printer endpoint PrintHub uses and prints a report. It changes nothing on
  the printer.
- On first start with no admin, `ADMIN_USERNAME` and `ADMIN_PASSWORD` create one. Remove them
  from `compose.yaml` afterwards; everyone else joins through invite links.

> The container runs as UID 10001 with a read-only root filesystem. `/data` must be writable by
> that UID; a named volume is, a bind mount has to be `chown`ed first.

Behind a reverse proxy that terminates HTTPS, such as `tailscale serve`, set `TRUST_PROXY=true`
so the session cookie is marked `Secure`, and origin checks and invite links use
`X-Forwarded-Proto` and `X-Forwarded-Host`.

## Configuration

| Variable | Default | |
|---|---|---|
| `PRINTER_HOST` | required | Printer hostname or IP address |
| `PRINTER_ACCESS_CODE` | `123456` | As set on the printer's touchscreen |
| `PRINTER_SN` | discovered | Serial number; set it if UDP discovery does not reach the printer |
| `PRINTER_NOZZLE` | `0.4` | `0.2`, `0.4`, `0.6` or `0.8`; selects the slicer profiles |
| `PRINTER_MQTT_PORT` | `1883` | |
| `PRINTER_UPLOAD_PORT` | `80` | |
| `PRINTER_CAMERA_PORT` | `8080` | |
| `LISTEN_ADDR` | `0.0.0.0:8080` | |
| `DATA_DIR` | `/data` | Database, uploads and sliced files |
| `TZ` | `UTC` | IANA zone the print schedule is written in |
| `ADMIN_USERNAME`, `ADMIN_PASSWORD` | unset | Creates the first admin; ignored once one exists |
| `TRUST_PROXY` | `false` | Honour `X-Forwarded-Proto` and `X-Forwarded-Host` |
| `CAMERA_ENABLED` | `true` | |
| `MAX_UPLOAD_MB` | `200` | |
| `SLICE_TIMEOUT` | `15m` | `90s`, `15m`, `1h` |
| `ESTIMATE_MARGIN` | `1.15` | Multiplies the print-time estimate when checking a job ends before a schedule window closes |
| `ORCA_SLICER` | `/opt/orcaslicer/bin/orca-slicer` | Without it, only G-code uploads are accepted |
| `ORCA_PROFILES` | `/opt/orcaslicer/resources/profiles/Elegoo` | |
| `RUST_LOG` | `info` for `serve`, else `warn` | Log filter, e.g. `debug` or `printhub=debug` |

## Build for another architecture

The image builds for `linux/arm64` and `linux/amd64`. Rust is cross-compiled on the build host,
so only the final `apt-get` step runs emulated.

```sh
docker buildx build --platform linux/arm64,linux/amd64 -t <registry>/printhub:latest --push .
```

## Development

Needs Rust 1.96 and `sqlx-cli`:

```sh
cargo install sqlx-cli --no-default-features --features sqlite
echo "DATABASE_URL=sqlite://$PWD/target/dev.db" > .env
set -a && . ./.env && set +a
sqlx database create
sqlx migrate run --source crates/printhub/migrations
```

- `sqlx::query!` checks queries against `DATABASE_URL` at compile time. After changing a query,
  run `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/`; the Docker build
  compiles from that offline data.
- `cargo run -p fakeprinter` starts an emulated printer (MQTT, upload, camera) and prints the
  `PRINTER_*` variables that point PrintHub at it. Then `cargo run -p printhub -- serve`.
- The test that runs the real slicer is skipped unless `ORCA_SLICER` and `ORCA_PROFILES` are set.
- `.claude/skills/verify-phase` holds the full check chain and a smoke test of `serve` against
  the fake printer.
