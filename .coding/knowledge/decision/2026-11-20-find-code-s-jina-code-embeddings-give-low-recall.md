+++
title = "find_code's jina code embeddings give low recall on plain-English questions; richer chunk text did not help"
created = "2026-11-20"
evidence = "Step 15's eval table in commit c6f47953; audit cqAuditc6f47953, section (6)."
+++

Measured 2026-11-20 on this repo (25 questions phrased without the symbol name, in tests/eval/find_code_questions.json; the #[ignore] eval in src/tool/agent/find_code.rs):
- find_code (0.6 cosine + 0.25 bm25 + 0.15 name) gets symbol recall@5 3/25 and file recall@5 11-12/25.
- The fair FTS baseline (a token-OR query mapped to enclosing symbols) gets 1/25 and 9/25.
- Adding split name words, the enclosing type and a 12-line body excerpt to the chunk text changed nothing (all variants 3/25).
- An audit confirmed the cosine term is live and consistent (same embedder and 512-token cap on both sides, fresh unit-normalised vectors), so this is a model/corpus outcome, not a bug.
- Implication: more chunk-text tuning won't lift recall. The next levers are a different embedding model (a natural-language-to-code retrieval model, or the memory model on doc-rich chunks), query rewriting, or re-weighting toward the name and doc terms. Measure any change with the same eval.
