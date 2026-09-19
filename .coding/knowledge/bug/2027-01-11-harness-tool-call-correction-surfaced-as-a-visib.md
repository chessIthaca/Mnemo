+++
title = "harness tool-call correction surfaced as a visible error event — FIXED (plan 995436c2)"
created = "2027-01-11"
+++

BUG: harness tool-call correction surfaced as a visible error event — FIXED (plan 995436c2). Symptom (user report 2027-01-24): on the second identical tool-call failure, the repeat-failure circuit breaker emitted AgentEvent::Error "tool-call correction: …" — a red transcript box, an activity-log row, and a doom-streak increment; the user wanted the correction in the LLM context ONLY. Root cause: src/agent/turn.rs sent the event alongside the (wanted) context injection. Fix: emission removed — context-only by design; the correction still rides the provider-facing messages. Regression test: repeat_failure_injects_schema_correction_on_second_identical_error (src/agent/tests.rs) asserts NO error event carries the text while the correction message IS in messages. Plan: .coding/plans/995436c2.md
