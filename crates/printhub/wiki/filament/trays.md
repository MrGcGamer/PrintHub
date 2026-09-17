---
title: Trays
summary: Telling PrintHub which spool is in which CANVAS tray, and when it forgets.
keywords: tray, trays, canvas, ams, load, bind, set spool, add spool, new spool, clear, refresh, mismatch, rfid, A1
order: 2
---
The printer reports whether each tray of its CANVAS holds filament, and a type and colour: read
from an Elegoo RFID tag, or as set on the printer for other spools. It cannot tell two spools of
the same filament apart, so after loading a spool, set it on the Filament page. A job starts
only from spools that are set on a loaded tray.

Trays are named **A1** to **A4**.

## Setting a spool

Only a tray that reports filament accepts a spool. The picker lists spools of the reported
material first, closest colour first.

A spool is in one tray at most: setting it on a second tray takes it out of the first, and
setting a tray replaces the spool that was there.

A tray holding filament no spool is recorded for offers *Add this as a spool*, which opens the
[new spool form](/wiki/filament/spools) with the material, brand and colour the printer reports
already filled in, and sets the spool on that tray once it is saved. The printer reports no
weight and no price, so those stay to be entered.

> Set the spool every time you load one, even the same kind as before. PrintHub cannot see a
> swap, and deducts the next print's filament from whichever spool is set.

## Refresh trays

PrintHub asks the printer for its trays every 30 seconds. *Refresh trays* asks now, so a spool
just loaded can be set straight away.

## Cleared automatically

When the printer reports a tray empty, PrintHub clears that tray's spool. If a whole CANVAS unit
disconnects, its spools stay set. *Clear* does it by hand, for when PrintHub missed a spool
coming out.

## Material mismatch

Shown when the type the printer reports for a tray is not the spool's material, compared as
text. It usually means the wrong spool is set, or the spool's material is written differently,
such as `PLA Silk` for a tray reporting `PLA`. The queue does not wait because of it.
