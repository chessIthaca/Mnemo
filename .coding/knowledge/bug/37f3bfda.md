+++
title = "Fix claude-opus-5 400: empty thinking block echoed into history (backlog bd5eb19d)"
created = "2026-12-29"
+++

Symptom: claude-opus-5 requests fail non-retryably with HTTP 400 "messages.3.content.0.thinking: each thinking block must contain thinking" — an empty thinking content block (thinking:"" with signature, captured from a stream whose thinking block never received a thinking_delta) is echoed verbatim into conversation history by the Anthropic provider's raw-turn echo, poisoning the conversation so every retry fails identically (bursts: provider-errors ids 1022-1030 ts 1788508444631-1788508460358 and 879-888, 2026-09-04). · regression test: build_request_json_drops_empty_thinking_block_from_echoed_history

Full record for plan 37f3bfda (see .coding/plans/37f3bfda.md for the plan file).

regression test: build_request_json_drops_empty_thinking_block_from_echoed_history · path .coding/plans/37f3bfda.md · branch wt/agenticcoding @ 5492a6b (unmerged — exists only on this branch)
