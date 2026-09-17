---
title: Printer
summary: The dashboard's live printer card, its controls and the camera.
keywords: dashboard, status, card, connection, connected, refused, temperature, pause, resume, stop, camera, snapshot, live view
order: 1
---
The dashboard shows the printer as PrintHub sees it. The card updates by itself; there is no
need to reload the page.

## Connection

The badge at the top of the card says whether PrintHub can talk to the printer.

| Badge | Meaning |
|---|---|
| Connected | Status and commands work. |
| Connecting | PrintHub is logging in to the printer. |
| Looking for the printer | The printer did not answer on the network, usually because it is off. PrintHub tries again every 30 seconds; switching the printer on is enough. |
| Refused: … | The printer answered but turned PrintHub away, and gives the reason. `too many clients` means other programs, such as a slicer, hold its connections. |
| Disconnected | The connection dropped. PrintHub reconnects on its own. |

While the printer is not connected, no job starts, and the temperatures and trays on the card
are the last ones it reported.

## The card

The heading is the printer's own state: Idle, Heating the bed, Printing, Paused, Finished and
so on. During a print the card adds the file, progress, layer and the printer's estimate of the
time left. Nozzle and bed temperatures add their target whenever one is set.

Below that, one line per CANVAS tray: the colour and material the printer reports, whether the
tray is empty, loaded or in use, and the spool PrintHub has in it. The nozzle size line is
explained under [Mounted nozzle](/wiki/printer/nozzle).

## Pause, resume and stop

Admins get these buttons on the card while the printer prints. Members pause, resume or stop a
print of their own job on the job's page.

Pause and resume only send the command; the card's heading shows when the printer has done it.
A stopped print cannot be resumed. Its job ends as Cancelled, and the filament it used up to
that point is charged to the spools, as described under
[How prints use filament](/wiki/filament/weighing#how-prints-use-filament).

## Camera

The live view is the printer's own camera. Everyone watching shares a single connection to it,
so more viewers put no extra load on the printer. *Open a still image* shows the current frame
alone, which is lighter on a phone connection. The queue page shows the same still next to the
button that [confirms the bed is clear](/wiki/queue/waiting#nobody-has-confirmed-that-the-bed-is-clear).
