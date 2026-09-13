+++
title = "tiktoken encoding is O(n²) per pretoken — one long run stalls compaction + turn accounting"
created = "2027-01-11"
+++

FOUND during plan 65af4c62 (compaction guard), step 2 — src/agent/context.rs.

Symptom: `cargo test --lib agent::context` never finished (only `build_summary_prompt_enforces_budget_with_marker` started, then burned CPU indefinitely). Not an infinite loop — a quadratic tokenizer cost.

Root cause: tiktoken-rs `encode_with_special_tokens` is O(n²) in the length of ONE pretoken. The cl100k pretokenizer regex `\p{L}+` matches an unbroken run (base64 blob, "x"*N, minified JSON) as a SINGLE piece, and tiktoken's byte_pair_encode/_byte_pair_merge rescans all adjacent parts per merge.

Measured on this machine (raw encode, single-piece runs): 1K=2.9ms, 2K=9ms, 4K=33ms, 8K=136ms, 16K=541ms, 32K=2.06s, 64K=8.32s (4x per doubling) ⇒ 128K≈33s, 400K≈5min. Mixed text is fine: 200K chars of "word " = 167ms.

Impact: any path that tokenizes a runaway tool result stalls on exactly the input it must handle — compaction budgeting, `count_message_tokens` turn accounting, and the summarizer prompt build.

Fix (shipped): `TOKEN_MEASURE_CHUNK_CHARS = 2048` + `floor_char_boundary` + `prompt_text_tokens` measures strings longer than the cap CHUNK-WISE and sums. Cost is linear; the sum is a conservative upper bound (no BPE merge spans a chunk split), which is the safe direction for budget guards. `count_message_tokens` now routes through `prompt_text_tokens` so turn accounting and summarization share ONE measure. Regression test: `prompt_text_tokens_stays_linear_on_one_huge_piece` (128K single piece, asserts < 20s — FAILS at ~33s without the fix; verified empirically with the cap temporarily set to usize::MAX).
