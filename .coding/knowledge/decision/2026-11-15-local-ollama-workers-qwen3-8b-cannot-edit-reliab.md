+++
title = "local Ollama workers — qwen3:8b cannot edit reliably, qwen3-coder:30b can"
created = "2026-11-15"
evidence = "workers localQwen01 (stopped) and localCoder30 (merged); review report .coding/reviews/team-d5dcf617-20261115-212759.md"
+++

Trial 2026-11-15 on an RTX 5090 (24 GB), same fully specified one-file task (three node tests in scripts/updater-manifest.test.mjs), same brief: qwen3:8b (Q4, 40k ctx) failed file_edit repeatedly by sending the post-change text as old_string, changed 2 lines only after a lead hint, and was stopped. qwen3-coder:30b (262k ctx) finished in about 2.5 min with no help, 8/8 tests, passed review first time. Local endpoint: ~/.mnemo/endpoints.toml "Ollama local" (http://localhost:11434/v1/, kind openai), listed as the second routine-tier model so defaults stay cloud. Mnemo2 only reloads endpoints/tiers on a settings save or restart. For small models, prefer file_append / line-number ops over exact old_string edits in briefs.

Amended 2026-11-15: Second trial, harder (in-place refactor in the middle of src-tauri/src/mnemo2/updates.rs: a new constant, a new helper, two call sites collapsed, both feature builds lint-clean): qwen3-coder:30b got the logic right and all checks passed, but one insert overwrote a neighbouring doc comment (the lead restored it with review_fix), and after finishing it kept sending the same "Task complete" report instead of ending its turn, so the lead stopped the session to review. Usable with review; watch for lost neighbouring lines and non-terminating turns. Speed on this machine: about 50 tokens/s output, about 640 tokens/s prompt reading at Ollama's default 262k context (45 GB loaded, more than the 24 GB GPU).
