---
title: Accounts
summary: Who can do what, invites, password resets, and the limits on changing admins.
keywords: account, user, users, admin, member, role, permission, permissions, invite, join, password, reset, forgot password, login, locked out, disable, session
order: 6
---
## Who can do what

There are two roles. Admins can do everything members can.

| | Member | Admin |
|---|---|---|
| Upload jobs; confirm, cancel, stop or retry them | own jobs | any job |
| Pause or resume a print | own job's print, from its page | any print, also from the dashboard |
| Move jobs in the queue | – | yes |
| Confirm the bed is clear | yes | yes |
| Add spools | as their own | for anyone, or shared |
| Edit, archive or restore a spool | own spools | any spool |
| Weigh in, set and clear tray spools | any spool | any spool |
| Record the mounted nozzle | if granted | yes |
| Record a payment | as the one paid | any |
| Delete a payment | – | yes |
| Print windows, users and invites | – | yes |

## Permissions

A permission lets a member do one admin-only thing. Admins tick them per member on the Users
page; admins hold all of them. There is one so far: *Record the mounted nozzle*, see
[Mounted nozzle](/wiki/printer/nozzle).

## Joining

New people join through an invite link from an admin, which makes them a member or an admin.
Usernames are 1 to 32 letters, digits, dots, dashes or underscores; passwords need at least 10
characters. Revoke a link that was sent to the wrong person under *Open links*.

## Forgotten passwords

An admin creates a *Password reset link* for the account, which works like an invite. Using it
sets the new password and logs the account out on every device.

## Logging in

A login lasts 30 days on each device. After 5 wrong passwords for one username, each further
attempt has to wait, starting at a second and doubling up to 15 minutes. A correct password
resets the count.

## What admins cannot change

- An admin cannot be made a member again, so make someone an admin with care.
- The last active admin cannot be disabled, and nobody can disable themselves.

Disabling an account logs it out everywhere and stops it logging in. Its spools, jobs and history
stay.
