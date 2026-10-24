---
title: RFC: Calendar Timer Scheduling
date: 2026-10-24
status: implemented
---

# RFC: Calendar Timer Scheduling

## Decision

Timers keep one lifecycle and one scheduler admission path. Existing relative
and fixed-interval records remain valid; calendar timers add an optional
`calendar` payload containing a normalized five-field cron expression and an
IANA timezone.

The runtime stores the next occurrence as `next_fire_at` in UTC, but derives it
from the calendar rule in the named timezone. After a fire, and when recovering
a record whose cached next occurrence is absent, the runtime asks the shared
calendar evaluator for the next matching local time. This preserves DST and
timezone semantics without treating a calendar schedule as a fixed UTC
interval.

## Compatibility and boundaries

* Legacy JSON records without `calendar` deserialize unchanged.
* Calendar creation requires both `calendar_expression` and `timezone`; the
  relative/interval fields are mutually exclusive with calendar fields.
* Calendar timer fires continue through the existing atomic timer-fire,
  queue-entry, wake, and admission transaction. No second execution loop is
  introduced.
* The accepted expression dialect is exactly five standard cron fields.
  Seconds, macros, and parser-specific extensions are rejected at the API
  boundary.
* The HTTP, CLI, local client, and `CreateTimer` agent tool expose the same
  optional fields; generated OpenAPI types are the source for GUI transport
  consumers.
