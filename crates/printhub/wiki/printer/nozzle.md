---
title: Mounted nozzle
summary: Why PrintHub has to be told the nozzle size, and what depends on it.
keywords: nozzle size, diameter, 0.2, 0.4, 0.6, 0.8, swap, change nozzle, record, hardened steel
---
The printer does not report which nozzle is fitted, so whoever swaps it records the new size
on the dashboard. Until somebody has, PrintHub assumes the size its server was set up with, and
the card says so.

## What depends on it

- **Slicing.** Models uploaded as STL are sliced for the recorded size, with the print and
  filament profiles made for it.
- **The queue.** A job whose G-code was sliced for another size waits until the recorded size
  matches; see [Sliced for another nozzle](/wiki/queue/waiting#sliced-for-another-nozzle).
  G-code that does not name its nozzle size is not checked.

> Record the new size straight after swapping the nozzle. The queue trusts the record: a job
> sliced for the old size would otherwise start on the new nozzle.

## Who may record it

Admins, and members an admin has given the permission *Record the mounted nozzle*, usually
whoever has the printer at their place. See [Permissions](/wiki/accounts#permissions).

## Choosing a size

Smaller nozzles print finer detail and take longer; larger ones print faster with thicker
layers. Each size's *Standard* print profile uses layers half as thick as the nozzle: 0.10 mm
for 0.2, up to 0.40 mm for 0.8.

Fibre- and wood-filled filaments clog less in larger nozzles. Fibre-filled ones also need a
hardened steel nozzle, which is what the Centauri Carbon 2 comes with; PrintHub records only the
size, so check the material of any other nozzle yourself. See
[Fibre-filled filament](/wiki/filament/materials/fibre-filled).
