+++
title = "Fix: 429 fallback permanently suppresses per-phase model overrides"
created = "2026-09-01"
+++

Symptom: Phase model switching no longer fires: after a single 429-triggered automatic fallback, the agent stays on the fallback provider forever — no switch to [models.planning]/[models.executing]/etc. on state transitions (e.g. plan→execute) or at conversation start. Root cause: try_429_fallback (src/agent/loop_impl.rs:1225, commit 914ca32) pins the fallback via set_explicit_provider; that pin outranks all state overrides and is never cleared. · regression test: state_override_survives_429_fallback

Full record for plan 0aeb92cd (see .coding/plans/0aeb92cd.md for the plan file).

regression test: state_override_survives_429_fallback · path .coding/plans/0aeb92cd.md · branch wt/agenticcoder @ 15bbbd6 (unmerged — exists only on this branch)
