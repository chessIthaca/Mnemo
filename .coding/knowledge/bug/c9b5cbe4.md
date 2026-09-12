+++
title = "Fix DeepSeek thinking-mode 400: reasoning_content must survive every wire path"
created = "2026-12-23"
+++

Symptom: DeepSeek thinking-mode requests (deepseek-v4-flash @ api.deepseek.com) fail non-retryably with HTTP 400 "The `reasoning_content` in the thinking mode must be passed back to the API" — recurring across ≥3 episodes (.coding/logs/provider-errors.jsonl ids 39-47, 48-56, 101), each a burst of ~9 rapid failures matching the 12-prompt auto-continue budget; latest 2026-12-23 at the finish→auto-continue transition of plan addf3727. Kills agent sessions; error is classified (reasoning_content_must_be_passed_back_is_serialization_bug) but the producing path is unfixed. · regression test: raw_echo_injects_reasoning_content_when_required_but_absent

Full record for plan c9b5cbe4 (see .coding/plans/c9b5cbe4.md for the plan file).

regression test: raw_echo_injects_reasoning_content_when_required_but_absent · path .coding/plans/c9b5cbe4.md · branch wt/agenticcoder @ b23463c (unmerged — exists only on this branch)
