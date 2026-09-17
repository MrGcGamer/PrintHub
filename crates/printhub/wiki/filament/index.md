---
title: Filament
summary: How PrintHub keeps track of spools, which one is in the printer, and how much is left.
keywords: inventory, spools, filament, grams, ledger, material name
order: 3
---
Every physical spool has a record in PrintHub. Its grams left change only through an entry in
the spool's history, a finished print, a stopped print or a weigh-in, so every change can be
traced back to a print or to someone with a scale.

## Material names

Write the material as its plain type, the way slicers name it: `PLA`, `PETG`, `ABS`, `PETG-CF`.
Three things compare it as text, ignoring upper and lower case:

- slicing a model picks the filament profile of that name, as described under
  [Filament profile](/wiki/queue/upload#filament-profile);
- the tray list warns when the printer reports another type, as described under
  [Material mismatch](/wiki/filament/trays#material-mismatch);
- statistics add up filament per material name.

Put the finish or product line, such as *Silk* or *Rapid*, in the colour name or the notes.
