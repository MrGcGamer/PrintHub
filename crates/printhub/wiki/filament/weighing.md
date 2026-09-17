---
title: Weighing and filament use
summary: How grams left are counted down after a print, and how a weigh-in corrects them.
keywords: weigh, weigh-in, scale, grams left, remaining, deduct, consumption, history, estimate, empty spool
order: 3
---
## How prints use filament

When a print ends, PrintHub deducts the grams its G-code states, for each filament from the spool
chosen for it:

- a finished print deducts all of it;
- a stopped or failed print deducts that share of it that matches its progress: 40 % done
  deducts 40 %. The history calls it *Unfinished print, estimated*.

Nothing is deducted while a print runs. The grams are the slicer's calculation, not a
measurement, so the figure drifts from the truth over many prints, and prints started without
PrintHub are not counted at all.

## Weigh-ins

A weigh-in sets the grams left to what the scale says. PrintHub records the difference: *used*
when there was less than counted, *found* when there was more.

Weigh a spool when the queue says it lacks filament you can see, before a long print on a spool
that has seen several, or whenever it is convenient. The weight of an empty spool differs by
brand; weigh an empty one of the same kind once and keep the number in the notes.

A weigh-in is nobody's printing. Its differences appear in the statistics under
[Not accounted for](/wiki/stats#not-accounted-for), never in someone's filament or balance.

## History

The spool's page lists every change, newest first, with who caused it: the job's owner for a
print, whoever weighed for a weigh-in.
