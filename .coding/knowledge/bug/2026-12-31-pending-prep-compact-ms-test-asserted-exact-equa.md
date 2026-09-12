+++
title = "pending_prep_compact_ms test asserted exact equality on a sliver-augmented value — timing-flaky"
created = "2026-12-31"
+++

BUG (found 2026-01-03 during plan 3fb064c4's full-suite run, pre-existing on clean HEAD 17fc6c2): `provider::openai::tests::pending_prep_compact_ms_stamp_the_next_record` failed intermittently-then-consistently with `left: Some(1265), right: Some(1234)`. Root cause: the test asserted EXACT equality on `rec.prep_ms`, but complete() intentionally stamps `parked_prep + record_created.duration_since(entry_at)` (the local sliver from complete() entry to record creation — body build + trace-log start, openai.rs ~:810-821, added by the trace-graph work 28cd957/4ddda37). On a slow machine the sliver crosses a millisecond boundary, so the exact assert is timing-flaky by construction. Fix: bounded-range assert `(1234..1234 + 10_000).contains(&prep)` (compact_ms carries no sliver — exact stays). Regression test: the corrected assertion in the same test (it failed with Some(1265) pre-fix on this machine, passes post-fix).
