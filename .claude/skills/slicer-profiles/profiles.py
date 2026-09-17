#!/usr/bin/env python3
"""Read OrcaSlicer vendor profiles the way a slice sees them: with `inherits` resolved.

Usage:
  profiles.py flatten NAME...                  full settings of each profile, as JSON
  profiles.py table SUBSTRING [KEY...]         one row per selectable profile whose name
                                               contains SUBSTRING (case-insensitive)
  profiles.py compatible MACHINE [KIND]        selectable KIND profiles (filament, process)
                                               naming MACHINE, raw vs. flattened

The vendor directory is $ORCA_PROFILES, else the mounted macOS DMG.
"""

import json
import os
import sys

DEFAULT_DIR = "/tmp/orca/mnt/OrcaSlicer.app/Contents/Resources/profiles/Elegoo"
TABLE_KEYS = [
    "filament_type",
    "nozzle_temperature",
    "nozzle_temperature_range_low",
    "nozzle_temperature_range_high",
    "cool_plate_temp",
    "textured_plate_temp",
    "hot_plate_temp",
    "fan_min_speed",
    "fan_max_speed",
    "filament_max_volumetric_speed",
]


def load(root):
    profiles = {}
    for directory, _, files in os.walk(root):
        for file in files:
            if not file.endswith(".json"):
                continue
            try:
                with open(os.path.join(directory, file)) as handle:
                    profile = json.load(handle)
            except (OSError, ValueError):
                continue
            if isinstance(profile, dict) and "name" in profile:
                profiles[profile["name"]] = profile
    return profiles


def flatten(profiles, name):
    """Leaf values win, like PrintHub's `ProfileLibrary::flatten`."""
    flat, seen = {}, set()
    while name:
        if name in seen:
            raise ValueError(f"inheritance loop at {name!r}")
        seen.add(name)
        profile = profiles[name]
        for key, value in profile.items():
            flat.setdefault(key, value)
        name = profile.get("inherits")
    flat.pop("inherits", None)
    return flat


def scalar(value):
    """Filament settings are one-element lists; show the element."""
    return value[0] if isinstance(value, list) and len(value) == 1 else value


def selectable(profile, kind=None):
    return profile.get("instantiation") == "true" and (kind is None or profile.get("type") == kind)


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    root = os.environ.get("ORCA_PROFILES", DEFAULT_DIR)
    if not os.path.isdir(root):
        sys.exit(f"no profile directory at {root}: mount the DMG or set ORCA_PROFILES")
    profiles = load(root)
    command, args = argv[1], argv[2:]

    if command == "flatten" and args:
        for name in args:
            print(json.dumps(flatten(profiles, name), indent=1))
    elif command == "table" and args:
        keys = args[1:] or TABLE_KEYS
        print("\t".join(["name"] + keys))
        for name in sorted(profiles):
            if args[0].lower() in name.lower() and selectable(profiles[name]):
                flat = flatten(profiles, name)
                print("\t".join([name] + [str(scalar(flat.get(key, "-"))) for key in keys]))
    elif command == "compatible" and args:
        machine, kind = args[0], (args[1] if len(args) > 1 else "process")
        for name in sorted(profiles):
            profile = profiles[name]
            if not selectable(profile, kind):
                continue
            raw = machine in profile.get("compatible_printers", [])
            try:
                flat = machine in flatten(profiles, name).get("compatible_printers", [])
            except (KeyError, ValueError) as err:
                print(f"skip\t{name}\t({err})")
                continue
            if raw or flat:
                print(f"{'raw' if raw else 'inherited'}\t{name}")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
