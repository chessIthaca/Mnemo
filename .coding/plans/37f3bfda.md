# Plan: Fix claude-opus-5 400: empty thinking block echoed into history (backlog bd5eb19d)

## Goal
Eliminate the non-retryable claude-opus-5 HTTP 400 "messages.3.content.0.thinking: each thinking block must contain thinking" by never capturing and never echoing an empty thinking/redacted_thinking content block: capture-side retain at message_stop finalization + echo-side sanitize in message_to_json's raw echo (shared is_always_invalid_thinking_block helper), with fails-without-fix regression tests for both paths, an analysis file modeled on the DeepSeek adjudication, and the BUG memory (881fef2d) superseded with the FIXED record.

## Kind
bug_fixing

## Context
Root cause verified in planning (src/provider/anthropic.rs): build_raw_content_block (851-884) initializes thinking:"" at content_block_start (signature may arrive in block start, 842-866); accumulate_raw_block_delta (890-934) only fills it via thinking_delta — a stream that never delivers one leaves the block empty; the message_stop finalization (684-714) parses tool_use inputs but never validates thinking blocks before emitting RawAssistantDelta; raw_content_is_usable (807-836) checks block types (and tool_use id/name/input) but never that a thinking block's thinking string is non-empty; message_to_json (379-503) echoes the raw array verbatim → Anthropic 400. Evidence status: the incident rows are GONE — .coding/logs/provider-errors.jsonl has rotated (current rows restart at id 1, 2026-08 era; incident ids 879-888/1022-1030 absent) and traces.jsonl does not exist in .coding/logs — so reproduction is synthetic, shape reconstructed from the error path (messages.3.content.0.thinking = empty thinking block at content[0] of an echoed assistant turn) + the capture code. Sibling family: DeepSeek reasoning_content echo bugs (BUG memories 1284933d, 8401fb46, 429a3d7b; adjudication method in .coding/analysis/deepseek-1042-shape.txt). Existing tests to keep green: build_request_json_echoes_raw_content_array_verbatim (1743), raw_captures_thinking_signature_text_and_tool_use (2607), assistant_reasoning_is_not_echoed_as_an_unsigned_thinking_block (2277). Historical constraint: filtering VALID thinking blocks caused 400 "Expected thinking or redacted_thinking, but found text" (Rule 4, comment at 416-421) — dropping an EMPTY thinking block is a different case: echoing it is a guaranteed 400, dropping it is never worse.

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
claude-opus-5 requests fail non-retryably with HTTP 400 "messages.3.content.0.thinking: each thinking block must contain thinking" — an empty thinking content block (thinking:"" with signature, captured from a stream whose thinking block never received a thinking_delta) is echoed verbatim into conversation history by the Anthropic provider's raw-turn echo, poisoning the conversation so every retry fails identically (bursts: provider-errors ids 1022-1030 ts 1788508444631-1788508460358 and 879-888, 2026-09-04).

## Regression test
build_request_json_drops_empty_thinking_block_from_echoed_history
