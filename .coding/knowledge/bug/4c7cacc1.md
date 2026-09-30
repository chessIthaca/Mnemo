+++
title = "Adapt the bundled embedder to fastembed 7 (unblocks dependabot PR #67)"
created = "2027-01-11"
+++

Symptom: Dependabot PR #67 (fastembed 4.9.1 -> 7.1.0) does not compile. Verified 2026-09-30: `cargo check --features embeddings` at that PR's head (6571f448) exits 101 with two errors in src/memory/embedder.rs — line 168 `fastembed::InitOptions::new(model)` uses an alias deprecated in 7.0.0 (fatal here because of the crate-wide #![deny(warnings)]), and line 223 `model.embed(vec![text], None)` on an `Arc<TextEmbedding>` fails with E0596 because `TextEmbedding::embed` now takes `&mut self`. · regression test: model_for_id_rejects_unknown

Full record for plan 4c7cacc1 (see .coding/plans/4c7cacc1.md for the plan file).

regression test: model_for_id_rejects_unknown · path .coding/plans/4c7cacc1.md · branch wt/mnemo @ 25619be (unmerged — exists only on this branch)
