# Plan: Fix DeepSeek thinking-mode 400: reasoning_content must survive every wire path

## Goal
Eliminate the recurring non-retryable DeepSeek 400 by guaranteeing every assistant message on a DeepSeek thinking-mode request carries its reasoning_content echo — present and non-empty — across ALL wire paths: mid-turn requests, finish→auto-continue transitions, session resume/rebuild, synthetic and summarized messages.

## Kind
bug_fixing

## Context
Evidence and findings so far: (a) Contract documented at src/provider/mod.rs:182-186 — DeepSeek thinking mode requires EVERY prior assistant message to echo reasoning_content; missing field = 400. ProviderPolicy.reasoning_required=true for DeepSeek (src/provider/policy.rs:175-179). (b) Builder paths in src/provider/openai.rs: real turns echo raw verbatim; the synthetic path (no raw, ~lines 1455-1465) re-adds reasoning_content with unwrap_or_default → EMPTY STRING when None; retention strip applies after (~1466-1480). (c) The retention strip is EXCLUDED as the trigger for id 101: strips_reasoning (policy.rs:122-133) returns false for DeepSeek below context pressure (age-0 always kept; historical_droppable=false → strips only under_pressure ≈ 307K tokens), and the failing session's prompt was ~97K. (d) Burst pattern = the auto-continue loop: while mid-turn requests in the same session succeeded, the requests that fail are the synthesized continue prompts after finish/complete — prime suspect is the history serialization for the auto-continue/resume path (agent.rs auto-continue budget; loop_impl resume; turn.rs assistant packaging ~1306/1484/1557/1587; event-log rebuild losing reasoning_content; synthetic re-add of None → ""). (e) No request bodies on disk: provider-errors.jsonl lines carry no request_json; .coding/logs/traces.jsonl does not exist. (f) Doc contradiction to reconcile: policy.rs:74-77 + mod.rs:746-755 + openai.rs:3943 test claim DeepSeek "tolerates dropping historical reasoning_content under pressure" vs the mod.rs:182-186 contract the live 400 enforces.

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
DeepSeek thinking-mode requests (deepseek-v4-flash @ api.deepseek.com) fail non-retryably with HTTP 400 "The `reasoning_content` in the thinking mode must be passed back to the API" — recurring across ≥3 episodes (.coding/logs/provider-errors.jsonl ids 39-47, 48-56, 101), each a burst of ~9 rapid failures matching the 12-prompt auto-continue budget; latest 2026-12-23 at the finish→auto-continue transition of plan addf3727. Kills agent sessions; error is classified (reasoning_content_must_be_passed_back_is_serialization_bug) but the producing path is unfixed.

## Regression test
raw_echo_injects_reasoning_content_when_required_but_absent
