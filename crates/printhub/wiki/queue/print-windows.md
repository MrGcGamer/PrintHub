---
title: Print windows
summary: Weekly rules for when jobs may start, and the details the rule form leaves out.
keywords: schedule, print windows, allow, deny, quiet hours, night, time, timezone, daylight saving, must finish, estimate, margin
order: 3
---
Admins set print windows under *Print windows*; that page explains allow and deny rules. What it
leaves out:

- A deny window that is open wins over an allow window open at the same time.
- A window keeps its clock times across a daylight saving change. A start time the change skips
  begins right after the gap.
- 00:00–00:00 covers a whole day.

## Must finish first

- The estimate comes from the job's G-code, plus a safety margin set on the server (15 % unless
  changed) for prints that run long.
- A job whose G-code has no estimate never starts while such a rule applies.
- Each window is judged on its own. Two allow windows that touch, such as Monday 18:00–00:00 and
  Tuesday 00:00–08:00, do not add up: a job started on Monday evening must finish by midnight.
- A job that would not finish in time does not hold up the queue: a shorter job behind it can
  start.

## Examples

| Goal | Rule |
|---|---|
| No prints still running at night | Deny, every day, 22:00–07:00, must finish first |
| Only print while someone is home on weekdays | Allow, Mon–Fri, 08:00–20:00 |
| No printing at weekends | Deny, Sat–Sun, 00:00–00:00 |
