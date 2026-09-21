+++
title = "bad-JSON retry loop re-injected identical schema advice (backlog 38040f12)"
created = "2027-01-11"
+++

Symptom: a tool call whose arguments need backslash escaping in JSON (e.g. search with pattern "\\(\\?<" to match the literal `(?<`) is rejected with "arguments malformed or truncated" and the retry re-emits the identical broken call. Live incident plan 263a9e31 (2026-09-21): FIVE consecutive identical failures on one search pattern before a hand-found paren-free reformulation ([(][?]) succeeded.

Root cause: src/agent/turn.rs::handle_bad_json (line 3229) pushed a FIXED per-call retry message (the "re-read the tool's schema, rewrite the COMPLETE call" text) on EVERY attempt up to MAX_BAD_JSON_RETRIES=8. That guidance is correct for the dropped-required-field class (plan 4fa222cc) but useless for a content-EMISSION failure — the model already knows the schema, and needs a DIFFERENT FORMULATION. Every retry was therefore byte-identical and the loop could not self-break. The strategy-changing machinery already existed (tool_call_correction, turn.rs:3506, via pending_correction at turn.rs:1644) but the bad-JSON path never consulted repeated_tool_failure nor set pending_correction.

Fix (plan 481be538, wt/macos-fix): handle_bad_json now computes the first-failure text via bad_json_retry_message() and, when repeated_tool_failure(messages, &tc.name, &failed_content) matches a PRIOR identical (tool, error) signature, substitutes repeated_bad_json_correction(&tc.name, ...) — which names a concrete alternative formulation per tool: search/search_read → `literal: true` (no regex escaping needed) or metacharacter-free classes like [(][?][<]; file_write/file_edit → chunk the body; default → rebuild the argument object from scratch. The scan runs BEFORE the current result is pushed (same ordering rule as the tool-execution circuit breaker). Guidance is idempotent from the second repeat on — the stuck state is the same, so the same remedy is correct; what must not happen is a reversion to the first-failure schema advice.

Regression test: src/agent/tests.rs::repeated_bad_json_for_same_tool_changes_strategy — drives three identical malformed `search` calls and asserts attempt 2's guidance differs from attempt 1's, attempt 3's does not revert to attempt 1's, and the repeat guidance names `literal`, plus round-2 hardening: harness attribution, idempotence, and the correct character class [(][?][<]. Fails before the fix (byte-identical guidance), passes after.

Related: DECISION c9bb3a5f (MAX_BAD_JSON_RETRIES=8), DECISION ab56df6d (repeat-failure detection), PLAN 4fa222cc (the circuit breaker + schema injection).
