---
title: Why a job waits
summary: Every reason a queued job has not started, in the order PrintHub checks them, and what to do.
keywords: waiting, blocked, not starting, stuck, reason, bed clear, offline, busy, empty tray, not enough filament, estimate
order: 2
---
A queued job starts as soon as nothing below applies. The queue page and the job's page show the
first reason that does. PrintHub checks again whenever the
printer, the queue or a setting changes, and every 30 seconds.

The first four hold up the whole queue. The rest hold up only the job they name, and jobs
behind it may start first.

## The printer is not connected

See [Connection](/wiki/printer#connection).

## Another job is printing

Or being sent to the printer. One job at a time.

## The printer is busy

The printer is not idle: it is printing something PrintHub did not start, or heating, levelling,
loading filament or updating. The job starts when the printer returns to idle.

## Sliced for another nozzle

*It was sliced for a 0.6 mm nozzle, but a 0.4 mm nozzle is mounted.* Either fit that nozzle and
[record it](/wiki/printer/nozzle), or cancel the job and slice it again for the nozzle that is
fitted.

## Nobody has confirmed that the bed is clear

PrintHub cannot see whether the last print is still on the bed, so every print that starts marks
the bed as not clear, and someone has to confirm it on the queue page after taking the print
off. The camera still there shows the bed as it is now.

> Confirm only once the bed is really empty. The next job starts within seconds, and a print
> left on the bed ends up under the nozzle.

> Fit the plate the next job was sliced for first: its job page names it under *Build plate*.

## The spool is not in a tray

The spool chosen for that filament is not [in any tray](/wiki/filament/trays). Load it and set it
on the Filament page. An archived spool is never in a tray.

## The tray is empty

The spool is set on a tray that the printer reports empty. Load the filament, or set the spool on
the tray it is actually in.

## Not enough filament

*Filament 1 needs 120 g, but its spool has 80 g left.* The figure is what PrintHub has counted,
not what is on the spool. If there is more than it thinks, [weigh the spool](/wiki/filament/weighing#weigh-ins).
Otherwise cancel the job and upload it again with a fuller spool.

## Print windows

The admins' [print windows](/wiki/queue/print-windows) say when jobs may start. The message
names the window:

- **No printing during “…”, until …**: a deny window is open.
- **Outside the print windows**: allow windows exist, and none is open. The message says when
  the next one opens.
- **It would not finish before …** and **“…” needs a time estimate**: the window requires jobs to
  finish in time. See [Must finish first](/wiki/queue/print-windows#must-finish-first).
