+++
title = "knowledge exports misdated by a wrong OS clock (floor fix withdrawn)"
supersedes = "2026-09-01-knowledge-exports-back-dated-by-stale-os-clock-f"
created = "2027-01-11"
+++

Exports once carried a stale-clock date (2026-09-01). The fix, a monotonic artifact-date floor, was later withdrawn: a future-dated file then pinned every new record to that future date. Current rule: see DECISION "knowledge records are dated by the wall clock only". No pinned clock exists in code; a wrong OS clock only misdates the records written while it is wrong.
