---
title: Print queue
summary: How a job moves from upload to finished, and what can be done with it on the way.
keywords: job, jobs, state, status, confirm, cancel, stop, retry, requeue, order, move, reorder, download, file, files, storage, disk, space, stl, gcode
order: 2
---
The queue page lists every unfinished job, in the order they will be tried, and the 20 that
ended most recently.

## Job states

| State | What is happening | What comes next |
|---|---|---|
| Slicing | PrintHub is turning an uploaded model into G-code. One model is sliced at a time; the rest wait their turn in this state. | Waiting for confirmation, or Failed. |
| Waiting for confirmation | The filament the job needs is known. Its owner has to choose the spools. | Queued. |
| Queued | Waiting for its turn and for everything it needs. The queue page says what it is waiting for. | Sending to the printer. |
| Sending to the printer | The file is being uploaded and started. | Printing. If the printer was busy or the transfer broke, Queued again, keeping its place. If the printer refused the file, Failed. |
| Printing | The printer is printing it. | Done, Cancelled if stopped, or Failed if the printer stopped without finishing. |
| Done, Cancelled, Failed | The job has ended. | A failed job can be retried. |

If PrintHub restarts in the middle of slicing, that job fails and the model has to be uploaded
again. A job that was being sent goes back to the queue.

## Confirming

After an upload, the job's page lists each filament the job uses, how many grams it needs, and
a spool picker for each. Spools of the same material come first, closest colour first. A model
sliced here has the spool it was sliced for already chosen.

Choosing a spool does not load it: before the job can start, the spool has to be
[in a tray](/wiki/filament/trays). If two filaments print from the same spool, the queue checks
that the spool holds enough for both together.

Spools cannot be changed once the job is queued. To print it from another spool, cancel it and
upload it again.

## Order

Jobs start in queue order, but a job that is waiting does not hold up the jobs behind it unless
what it waits for holds up everyone: the printer being offline or busy, another job printing,
or the bed not being confirmed clear. A job waiting for its spool is overtaken by one whose
spool is ready.

Admins move queued jobs up and down with the arrows. A job queued again, by a retry, goes to the
back.

## Files

A job's page links its files under *Files*: the model as uploaded, and the G-code, once there is
some. They download under the name the job was uploaded with. Anyone logged in can take them,
which is the way to reslice someone else's model yourself or to check what the printer was sent.

The printer holds a job's G-code only while it prints: PrintHub deletes it there once the print
ends, and sends it again for a retry. PrintHub itself keeps the files until an admin deletes them
under *Storage*. The job stays in the history, but without its files it can no longer be
downloaded or retried.

## Cancelling and stopping

*Cancel* removes a job that has not reached the printer. A job being sent to the printer cannot
be cancelled; wait until it prints, then stop it. *Stop* on a printing job stops the printer; the
job ends as Cancelled once the printer reports it has stopped.

## Retrying

A failed job that has G-code can be retried with the same spools. It goes to the back of the
queue. A model whose slicing failed has no G-code; read the error on its page, which ends with the
slicer's own last lines when the slicer failed, then upload it again.
