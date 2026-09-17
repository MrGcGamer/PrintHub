---
name: slicer-profiles
description: Look up what PrintHub's bundled OrcaSlicer profiles actually set for the Centauri Carbon 2 — temperatures, plate temperatures, fan, flow, compatibility — with `inherits` resolved. Use when checking a slicing fact, writing material docs in the help wiki, or debugging which profiles the upload form lists or what a sliced model prints with.
---

# Inspect the slicer profiles

A bundled profile file holds only what differs from its parent, so reading one file gives the
wrong answer: `0.12mm Fine @Elegoo CC2 0.4 nozzle.json` has no `compatible_printers`, and
filament leaves have no bed temperatures. `profiles.py` resolves `inherits` the way
`slicer::ProfileLibrary::flatten` does (leaf values win).

## 1. Get the profiles

On this Mac, from the OrcaSlicer 2.4.2 DMG (re-download from the `OrcaSlicer/OrcaSlicer` GitHub
release if `/tmp` was cleared):

```sh
mkdir -p /tmp/orca/mnt
hdiutil attach -nobrowse -readonly -mountpoint /tmp/orca/mnt /tmp/orca/OrcaSlicer_Mac_universal_V2.4.2.dmg
```

`profiles.py` reads `/tmp/orca/mnt/OrcaSlicer.app/Contents/Resources/profiles/Elegoo` unless
`ORCA_PROFILES` points elsewhere, such as the unpacked AppImage's
`resources/profiles/Elegoo`. The image ships the Linux AppImage of the same version; check a
fact against it when the answer matters for production.

## 2. Ask

```sh
S=.claude/skills/slicer-profiles/profiles.py
$S table "@ECC2"                                  # every CC2 filament: temps, plates, fan, flow
$S table "PLA" nozzle_temperature cool_plate_temp # chosen keys only
$S flatten "Elegoo PETG @ECC2"                    # everything one profile sets
$S compatible "Elegoo Centauri Carbon 2 0.4 nozzle" process
$S compatible "Elegoo Centauri Carbon 2 0.4 nozzle" filament
```

- `table` and `compatible` list only selectable profiles (`"instantiation": "true"`).
- Filament values are one-element lists; `table` prints the element.
- `compatible` marks each hit `raw` (the file names the machine) or `inherited` (only the
  flattened profile does). PrintHub's upload form lists both.

## 3. Before stating a fact

- Profiles say what the slicer is told, not what a slice produces. For anything PrintHub or the
  CLI sets at slice time, such as the plate type (`curr_bed_type`, chosen on the upload form;
  the CLI's own default is Cool Plate), read real G-code: a fresh slice via
  `slicer::tests::real_slicer_when_available` (command in `CLAUDE.md`). The fixtures in
  `crates/printhub/tests/fixtures/` predate the plate choice and say Cool Plate.
- Plate columns are named by OrcaSlicer: `cool_plate_temp` Cool Plate, `hot_plate_temp` High
  Temp Plate, `textured_plate_temp` Textured PEI Plate. A value of 0 means the filament does not
  support that plate (`PrintConfig.cpp` tooltips).
- The user swaps between a textured PEI and a smooth High Temp plate, so a material fact about
  the bed has to name the plate.
