+++
title = "typed durable memory records are timeless in recall scoring"
created = "2026-11-08"
+++

In `MemoryStore::recall_peek` (src/memory/mod.rs, scoring closure), rows whose stored `record_type` is SPEC, DECISION, BUG or HOW skip BOTH the strength decay and the staleness penalty: a typed durable record is true until superseded, so age says nothing about it. Session digests, plan digests, review digests and untyped facts still age. Weights, the derived per-class cap and `strongest` are unchanged.

Why it was needed: age was worth up to 0.55 in the score (0.3 decayed strength + 0.25 staleness) against a perfect semantic match's 0.35, so a durable record nobody had searched for lost to an unrelated row touched that day. With the exemption, the memory eval's title control (a record asked by its own title) reached hit@5 1.00 (was 0.65) and paraphrased hit@5 0.40 (was 0.10); noise@5 fell from 0.485/0.395 to 0.22.

The class signal is the row's stored `record_type`, not a re-parse of the title: the column agrees with the title prefix on every live row, and the write path derives record_type from the title at insert time, so knowledge-derived rows carry their typed title too.

Regression test: `memory::tests::old_typed_knowledge_beats_a_fresh_unrelated_fact` fails with the exemption removed (an unrelated fresh row outranks a 90-day-old SPEC record) and passes with it; `old_knowledge_keeps_its_relevance_order`, `old_episodic_digests_still_age` and `old_untyped_facts_still_age` pin the edges.
