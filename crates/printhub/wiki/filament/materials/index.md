---
title: Materials
summary: Which filament suits a print, what the Centauri Carbon 2 can print, and how to read the settings tables.
keywords: filament types, which filament, choose material, compare, guide, temperature, settings
order: 4
---
## Choosing

| The print needs to | Material |
|---|---|
| look good indoors: models, prototypes, toys, figures | [PLA](/wiki/filament/materials/pla) |
| look special: silk, matte, translucent, glitter, glow, wood | [PLA finishes](/wiki/filament/materials/pla-finishes) |
| take load and water: brackets, clips, holders, below 80 °C | [PETG](/wiki/filament/materials/petg) |
| survive sun and heat outdoors, up to 93 °C | [ASA](/wiki/filament/materials/asa) |
| take heat and knocks cheaply, or be smoothed with acetone | [ABS](/wiki/filament/materials/abs) |
| stay tough and strong in heat | [PC](/wiki/filament/materials/pc) |
| wear and slide: gears, parts that bend without breaking | [Nylon](/wiki/filament/materials/nylon) |
| flex: washers, tyres, soles, phone sleeves | [TPU](/wiki/filament/materials/tpu) |
| keep its shape better than the plain material | [Fibre-filled](/wiki/filament/materials/fibre-filled) |

When in doubt, start with PLA or PETG, the two easiest to print.

## What the Centauri Carbon 2 prints

According to Elegoo, the nozzle reaches 350 °C and the bed 110 °C, the nozzle is hardened steel,
and the printer is enclosed. Every material in this guide has a profile for it in the slicer
PrintHub uses.

No filament is recommended for dishes or anything else in contact with food: the grooves between
layers hold bacteria, whatever the material.

## Reading the settings tables

Each material page lists the Centauri Carbon 2 filament profiles of OrcaSlicer 2.4.2, the slicer
and version PrintHub runs, named without their `@ECC2` or `@Elegoo` suffix. *Elegoo* profiles are
tuned for Elegoo's filament; *Generic* ones are a starting point for other brands. For another
brand, compare the table with the temperatures on its label.

| Column | Meaning |
|---|---|
| Nozzle | Printing temperature in °C, and in brackets the range the profile allows. |
| Bed | Bed temperature in °C for a textured PEI plate / a High Temp plate, as OrcaSlicer names the plate types. A model sliced here uses the one for the [build plate](/wiki/queue/upload#build-plate) chosen when uploading. |
| Fan | The part-cooling fan's range in percent. |
| Flow | The most plastic per second, in mm³, the profile lets the nozzle melt. It caps the print speed, and is what *Rapid* and *HF* profiles raise. |

Sources for this guide: the profiles themselves, Elegoo's
[Centauri Carbon 2 Combo FAQ](https://wiki.elegoo.com/faq/centauri-carbon-2-combo) and
[drying guide](https://wiki.elegoo.com/filaments/fdm-filament-drying-and-storage-guide), and
Prusa's [material guide](https://help.prusa3d.com/filament-material-guide).
