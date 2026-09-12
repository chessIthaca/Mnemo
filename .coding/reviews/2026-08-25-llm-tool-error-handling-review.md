## Verdict: FINDINGS (0 high, 4 low)

Reviewed ALL uncommitted changes on `feat/llm-tool-error-handling` (`git diff HEAD`): 9 tracked files + 3 untracked. The three coupled changes (backend bad-JSON cap, frontend merge-chain break, frontend error summary) are **correct** — no correctness bugs, no security issues, no constitution violations in the source code. The findings are all low-severity documentation/hygiene issues.

---

### Correctness verification (all PASS)

**1. `bad_json_count` correctness — PASS.** `src/agent/turn.rs:1151-1232`:
- Increments only inside the `has_bad_json` block (line 1157); that block `continue`s (line 1228), so it **skips** the reset at line 1232. ✓
- Resets to 0 at line 1232, reached only when `has_bad_json == false` (valid JSON produced), placed before the `tool_calls.is_empty()` check (line 1233). ✓
- Never touches `tool_error_count` — the `has_bad_json` block no longer increments it (the old `tool_error_count += 1` was replaced by `bad_json_count += 1`). ✓
- **Mixed sequence [bad-JSON, bad-JSON, tool-exec-failure]**: bad_json_count goes 1→2, tool_error_count stays 0; then valid JSON resets bad_json_count=0 and the tool executes → fails → tool_error_count=1. `1 < MAX_RETRIES(3)` → no abort. Verified by code reading. ✓
- **Null-turn reset**: a null turn (valid JSON, empty tool_calls) resets bad_json_count to 0, but the turn **returns** immediately (line 1262) — it does not loop back. `bad_json_count` is a per-`run_turn` local (declared at line 115), so it cannot leak across turns. No loophole. ✓

**2. `MAX_RETRIES` test — PASS.** `src/agent/tests.rs:1130` (`max_retries_aborts_after_consecutive_tool_errors`):
- Uses `fragment: r#"{"path":"does_not_exist.txt"}"#` — **valid JSON**, so `has_bad_json == false`. The `file_read` tool executes and returns `success:false` (non-existent path). This is a genuine tool-EXECUTION failure, not bad-JSON. ✓
- The cap check at `turn.rs:1696` (`if tool_error_count >= MAX_RETRIES`) fires **after** the per-call loop (lines 1298-1661) feeds each result back. Each response has one tool call: response 1 → tool_error_count=1 + 1 tool msg; response 2 → 2 + 2 msgs; response 3 → 3 + 3 msgs, then the post-loop cap trips. The 4th response is never consumed. The assertion `tool_msgs.len() == 3` is correct. ✓

**3. `canMerge` guard — PASS.** `frontend/src/hooks/agentEventReducer.ts:444-454`:
- `result === null` (running/parallel call): `lastCallFailed` requires `lastCall.result !== null`, so it's `false` → merges. ✓ (covered by the existing "merges consecutive same-tool calls" test at line 237, which sends two starts with no result between them)
- Successful call: `!lastCall.result.success` is `false` → `lastCallFailed=false` → merges. ✓ (test at line 277)
- Completed error: `lastCall.result !== null && !lastCall.result.success` → `lastCallFailed=true` → breaks chain. ✓ (test at line 252)
- `last.calls[last.calls.length - 1]` when `last.calls` is empty: JS returns `undefined` (no crash); `lastCallFailed` becomes `false`. A tool entry is always created with ≥1 call, so empty is unreachable anyway. ✓

**4. `toolErrorSummary` — PASS.** `frontend/src/lib/toolCardPaths.ts:195-203`:
- Empty/whitespace input: `.find(l => l.length > 0)` returns `undefined` → returns `""`. ✓
- Exactly 120 chars: `firstLine.length <= 120` → returned verbatim, no ellipsis. ✓
- >120 chars: `slice(0, 119) + "…"` = 120 total (for non-whitespace input). ✓
- All 6 table tests in `toolCardPaths.test.ts:177-213` match the implementation. ✓

**5. ToolCard guard + raw output — PASS.** `frontend/src/components/chat/Message.tsx:597-601`:
- `failed` (line 452) is defined as `!running && lastCall.result !== null && !lastCall.result.success`, so when `failed` is true, `lastCall.result` is guaranteed non-null. The `&& lastCall.result` guard is redundant-but-safe TypeScript narrowing. ✓
- `lastCall.result.output` is the **raw** output: confirmed at `turn.rs:1481-1504` — the `[tool error]` prefix is added only to `tool_content` (the LLM-bound tool message), while `AgentEvent::ToolResult` carries `result.clone()` (raw `ToolResult`). The frontend stores `event.result` verbatim (`reduceToolResult`, agentEventReducer.ts:544). So no `[tool error]` prefix leaks into the summary. ✓ The doc comment's claim is accurate.

**6. Test adequacy — PASS.** Three Rust tests (`max_retries_aborts_after_consecutive_tool_errors`, `bad_json_retries_continue_past_three`, `bad_json_aborts_at_higher_cap`) prove the counter separation and both caps. The unchanged `error_recovery_malformed_json` (tests.rs:901) still passes (single bad call → retry hint + sanitized args). Two frontend merge tests + six `toolErrorSummary` table tests cover the UI changes. The `bad_json_aborts_at_higher_cap` test correctly asserts 7 tool messages (calls 1-7 feed back; the 8th trips the cap before pushing a message) and `bad_json_retries_continue_past_three` correctly asserts 4 tool messages + no final error.

---

### Findings

#### Low 1 — Stale HOW knowledge file (documentation sync)

**File:** `.coding/knowledge/how/2026-08-25-three-strikes-error-caps-tool-card-error-display.md:7`

This untracked file (part of the uncommitted changes) is the canonical reference for the error caps, but it was **not updated** to reflect this very change. Line 7 states:

> `MAX_RETRIES=3` ... Incremented in TWO places: (a) turn.rs:~1146 `has_bad_json` block ... (b) turn.rs:~1405 tool-execution failure

After this change, `MAX_RETRIES` (`tool_error_count`) is incremented in **ONE** place only — the tool-execution failure (b). The `has_bad_json` block (a) now increments `bad_json_count` toward `MAX_BAD_JSON_RETRIES`, not `tool_error_count`. The file also lists "Three 'three-strikes' caps" but there are now **four** — the new `MAX_BAD_JSON_RETRIES=8` is missing entirely.

**Fix:** Update the HOW file: (1) correct cap #1 to say `tool_error_count` is incremented in ONE place (tool-execution failure, `result.success==false` unless denial); (2) add a fourth cap entry for `MAX_BAD_JSON_RETRIES=8` (bad-JSON, incremented in the `has_bad_json` block, resets on valid JSON, aborts at turn.rs:1158). The source-code doc comments in `src/agent/mod.rs:36-59` ARE correct — only the knowledge file is stale.

#### Low 2 — `toolErrorSummary` doc/caller mismatch (empty summary not hidden)

**File:** `frontend/src/lib/toolCardPaths.ts:187` (doc comment) vs `frontend/src/components/chat/Message.tsx:597-601`

The `toolErrorSummary` doc comment states: *"Returns an empty string for empty/whitespace-only input (the caller hides the summary line when empty)."* But the caller does **not** hide it — `Message.tsx` renders `{toolErrorSummary(lastCall.result.output)}` unconditionally when `failed && lastCall.result`. If a failed tool result has empty/whitespace-only `output`, an empty `<div>` (with `mt-[0.3em]` margin, no text) renders — a minor cosmetic artifact. In practice failed results almost always carry non-empty output, so this is unlikely to manifest.

**Fix (either):** (a) make the caller honor the contract — `{failed && lastCall.result && toolErrorSummary(lastCall.result.output) !== "" && (...)}`; or (b) correct the doc comment to drop the "the caller hides" claim. Option (a) is preferable (matches the documented contract).

#### Low 3 — Stray unrelated changes mixed into the branch

**Files:** `.coding/knowledge/spec/2026-08-24-suppress-webview-context-menu-in-mnemo-ui-except.md` (modified: added `status = "superseded"`) and `.coding/knowledge/spec/2026-08-24-suppress-webview-context-menu-in-mnemo-ui-except-2.md` (untracked duplicate).

These webview-context-menu spec changes are **unrelated** to the LLM tool-error handling plan goal. They appear to be leftover noise from a prior/different effort. Committing them with this feature's changes muddies the commit's purpose.

**Fix:** Either revert/drop these two files from this commit (commit them separately if intentional), or confirm they belong here and document why. At minimum, exclude them from the feature commit.

#### Low 4 — PLAN.md does not mention the new bad-JSON cap

**File:** `PLAN.md:258`

> A retry cap (`MAX_RETRIES = 3`) prevents infinite loops; when hit, the agent surfaces an `AgentEvent::Error` and pauses.

This is still technically accurate (`MAX_RETRIES=3` still exists for tool-execution errors), but it's now **incomplete** — there is a second, higher cap (`MAX_BAD_JSON_RETRIES=8`) for LLM-produced malformed tool-call arguments, which is a deliberate design decision (the model can self-correct). PLAN.md is the project's technical-decisions doc and should note both caps.

**Fix:** Add a sentence noting that malformed tool-call JSON uses a separate, higher cap (`MAX_BAD_JSON_RETRIES=8`) because the model can recover by emitting valid JSON, distinct from tool-execution failures (`MAX_RETRIES=3`).

---

### Constitution compliance

- **Doc comments on public items:** `MAX_BAD_JSON_RETRIES` (`src/agent/mod.rs:49-59`) and `toolErrorSummary` (`frontend/src/lib/toolCardPaths.ts:181-194`) both have doc comments. `MAX_RETRIES` doc updated to clarify it counts tool-EXECUTION failures. ✓
- **No `#[allow(...)]`:** Searched the full diff — zero `#[allow` attributes introduced. ✓
- **`#![deny(warnings)]`:** No new warnings possible from the changes (a new `use super::MAX_BAD_JSON_RETRIES` import that is used; a new `let mut bad_json_count` that is read). The plan reports `cargo test` was green. ✓
- **Multi-platform neutrality:** No Windows-only APIs, paths, or shell syntax in any changed file. The changes are pure Rust logic + TypeScript. ✓
- **Regression tests:** Added for every behavior change (3 Rust + 2 TS merge + 6 `toolErrorSummary` table tests). ✓
- **Branch policy:** Changes are on `feat/llm-tool-error-handling`, not `main`. ✓
