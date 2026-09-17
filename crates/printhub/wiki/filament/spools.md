---
title: Spools
summary: What each field of a spool is for, who owns it, and when to archive it.
keywords: spool, add spool, edit, owner, shared, price, cost, archive, restore, colour, brand, full spool weight
order: 1
---
## Fields

**Material**: see [Material names](/wiki/filament#material-names).

**Brand** and **colour name** only label the spool in lists.

**Colour** ranks spools in the tray and confirmation pickers, closest first, and colours a model
sliced here in the G-code.

**Filament on a full spool**: the weight of the filament alone when the spool was new, usually
printed on the label as net weight. A new spool starts with this much left. Changing it later
does not change the grams left; only a [weigh-in](/wiki/filament/weighing#weigh-ins) does.

**Price**: what the full spool cost, without a currency; use the same currency on every spool.
With a price, filament others print from the spool is worth *price × grams ÷ full spool* and
counts towards [balances](/wiki/stats/balances). Without one, printing from it costs nobody
anything.

## Owners

A spool belongs to whoever added it. Admins can give it to anyone, or make it **Shared**: bought
together, so printing from it is nobody's debt. Who may change which spool is listed under
[Who can do what](/wiki/accounts#who-can-do-what).

Editing a spool's owner or price changes only filament used afterwards. What was already used
keeps the owner and value it had at the time.

## Archiving

Archive a spool once it is empty or gone. It leaves its tray and the spool pickers, and its
history and statistics stay. *Show archived spools* on the Filament page lists it again, and
*Restore* brings it back.
