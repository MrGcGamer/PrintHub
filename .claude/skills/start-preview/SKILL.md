---
name: start-preview
description: Start (or stop) PrintHub on this Mac — the built Docker image on http://localhost:8080 against the fake printer. Use when asked to start, run, open or restart PrintHub locally, the preview, or localhost:8080.
---

# Start the PrintHub preview

```sh
.claude/skills/start-preview/preview.sh        # start or restart
.claude/skills/start-preview/preview.sh stop   # remove the containers, keep the data
```

Log in at http://localhost:8080 as `admin` / `printhub-preview-1`. Data lives in the
`printhub-preview-data` volume and survives restarts; delete that volume for a fresh start.

The script handles what went wrong before:

- Colima reporting `Running` with a dead VM after the Mac slept: it force-stops and restarts.
- A missing fake-printer binary: it rebuilds it into `printhub-smoke-bin`.
- Stale printer ports: the two containers are always recreated together.

It runs whatever `printhub:arm64` currently is. After changing the code, rebuild the image
first, or the preview shows old behaviour:

```sh
docker buildx build --platform linux/arm64 -t printhub:arm64 --load .
```

The fake printer's camera sends an 8-byte placeholder, so the camera image stays blank.
