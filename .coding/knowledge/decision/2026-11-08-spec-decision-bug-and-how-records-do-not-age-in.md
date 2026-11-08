+++
title = "SPEC, DECISION, BUG and HOW records do not age in recall"
created = "2026-11-08"
evidence = "memory::tests::old_typed_knowledge_beats_a_fresh_unrelated_fact (red before the fix); mnemo2_memory_eval on a frozen store copy, with and without the change"
+++

In `MemoryStore::recall_peek` (src/memory/mod.rs), typed durable records skip both strength decay and the staleness penalty: a record is true until superseded, so age says nothing about it. Session digests, plan and review digests and untyped facts still age, which is what the staleness penalty was added for. Age had been worth up to 0.55 (0.3 decayed strength plus 0.25 staleness) against a perfect semantic match's 0.35, so a record nobody had searched for lost to an unrelated row touched today; asked by its exact title, a record reached the top five only 65% of the time. This was the largest single gain in recall: paraphrased hit@5 0.100 to 0.400, exact-title hit@5 0.650 to 1.000. The class signal is the row's stored record_type. Do not reintroduce decay for these types without rerunning mnemo2_memory_eval.
