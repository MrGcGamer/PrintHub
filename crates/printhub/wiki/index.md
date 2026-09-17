---
title: How PrintHub works
summary: The path a print takes, from a spool on the shelf to a line in the statistics.
keywords: overview, introduction, start, basics, workflow
---
PrintHub shares one Elegoo Centauri Carbon 2 between a group of people. It keeps a queue, knows
whose filament is in the printer, starts prints when nothing stands in the way, and adds up who
printed with whose filament. Everything in this help hangs off one path:

1. **Add a spool** to the inventory, with its weight and, if others will print from it, its
   price. See [Spools](/wiki/filament/spools).
2. **Put the spool in a tray** of the CANVAS and tell PrintHub which spool it is. The printer
   reports that a tray holds filament, not whose. See [Trays](/wiki/filament/trays).
3. **Upload a job**: a model PrintHub slices, or G-code sliced on your own computer. See
   [Uploading a job](/wiki/queue/upload).
4. **Confirm the job** by choosing the spool each of its filaments prints from. See
   [Confirming](/wiki/queue#confirming).
5. **The queue starts it** once the printer is free, the right nozzle is fitted, someone has
   confirmed the bed is clear, the spools are loaded with enough filament, and the print
   windows allow it. See [Why a job waits](/wiki/queue/waiting).
6. **The print ends** and PrintHub deducts its filament from the spools. See
   [Weighing and filament use](/wiki/filament/weighing).
7. **Statistics** show who printed how much with whose filament, and what that leaves people
   owing each other. See [Balances](/wiki/stats/balances).

PrintHub starts only the prints it queued. A print started on the touchscreen or from a slicer
keeps the queue waiting until it ends, and its filament is noticed only when someone weighs the
spool.

Look for the round **?** next to a heading or field anywhere in PrintHub: it opens the part of
this help that explains it.
