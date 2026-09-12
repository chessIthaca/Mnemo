# Review: auto-recall cache invalidation removal (fix/auto-recall-cache-invalidation)

**Scope:** all uncommitted changes vs `HEAD` — `src/agent/turn.rs` (invalidation removed, comments updated), `src/agent/tests.rs` (new regression test, +110 lines), plus bookkeeping (`.coding/backlog.json`, `.coding/plans/stack.json`, untracked `.coding/analysis/memverify.py`, two `.coding/plans/*.md`).

## Verdict

The core change is **correct and well-justified**. One minor finding (stale comment in the pre-existing test). No correctness, bug, or security findings.

## Verification detail

### 1. Correctness of the removal — PASS

- **Summarize invalidation intact:** `src/agent/turn.rs:324-326` still sets `last_recalled_user_query = None; last_recall_results = None;` inside the did-summarize branch. Correct to keep — the summarizer rewrites the message prefix and can replace the latest user message.
- **No dead/missing variable uses after the removal:** `last_recalled_user_query` / `last_recall_results` are declared at turn.rs:147-148, written by the summarize invalidation (325-326) and by the two cache-store arms (393-394, 404-405), and read by the cache-hit check (386-387). The removed block at ~1305 was the only other writer. All reads/writes remain consistent; no dead code, no unused-variable warnings.
- **The query truly cannot change on a tool-result append:** the recall query is built at turn.rs:371-375 via `messages.iter().rev().find(|m| m.role == Role::User)` — the only `Role::User` reference in turn.rs. Tool results are pushed as `Role::Tool` (turn.rs:1283-1291). Steers/suggestions are injected as `Role::System` (turn.rs:285-294 summarize-buffered path; src/runtime/agent.rs:625-632 runtime path), so they never enter the query either. (A mid-turn `Prompt` command does push a User message at the runtime level, but that is between-turns, and even hypothetically the cache is keyed by query *text* — a changed query naturally misses the cache and triggers a fresh recall at the 386 comparison. No staleness path exists.)
- **Event-suppression logic already guards reuse:** `fresh_recall` (turn.rs:384) means `MemoryRecalled` fires only on fresh recalls (416); with the invalidation gone, iteration 2 reuses the cache and emits nothing — exactly the intended behavior.

### 2. Regression test genuinely discriminates — PASS

`src/agent/tests.rs:3985-4093` `large_tool_result_does_not_reemit_memory_recalled` mirrors the proven-green `auto_recall_emits_memory_recalled_event` harness (same store seeding, same `MockProvider::sequence` two-round shape, same "merge it" query). Reasoned on OLD code: round 1 fresh recall → event #1; the ~8.2 KiB line-numbered `file_read` output (> 4096) clears the cache → round 2 fresh recall → event #2 → `assert_eq!(recalled, 1)` fails. On NEW code: round 2 hits the cache (`fresh_recall == false`) → exactly 1 event. The precondition is asserted at runtime (`large_result_seen` via `result.output.len() > 4096`, tests.rs:4074-4079), so the test cannot silently pass without a large result. The `assert_eq!(outcome.text, "ok")` guard confirms both iterations ran.

### 3. Comments / stale references — one MINOR finding

- **MINOR (docs accuracy):** `src/agent/tests.rs:3880-3884` — the comment on the *pre-existing* `auto_recall_emits_memory_recalled_event` test still explains its design via the now-removed behavior: "round 1 is a small file_read (result < 4096 bytes, so the recall cache is NOT invalidated)". Post-fix, tool results never invalidate the cache regardless of size, so the parenthetical rationale is stale (it was missed by the comment-update pass). Suggested rewording: "round 1 is a small file_read (under the OLD code a large result would have invalidated the recall cache; kept small so this test isolates the cache-reuse path)". The test itself still passes and remains valid.
- Checked every other grep hit for `4096` / "large tool-result": `src/agent/context.rs:420` is about the tiktoken BPE cache and `count_tokens` re-invocation — still accurate (the separate 8 KiB token-estimate full-recount path at turn.rs:207-209 remains). Remaining hits (`provider/openai.rs` max-output/context-length/buffer sizes, `tests.rs:3216` mock config, `trace.rs` fixture) are unrelated constants. The three new comment blocks in turn.rs (141-146, 360-369, 1308-1315) are accurate and correctly scoped.

### 4. Constitution compliance — PASS

- No new public API (the test fn is private); doc-comment rule N/A for the new code, existing comments follow style.
- No `#[allow(...)]` added anywhere in the diff.
- No dead code introduced (see §1); `result` is still consumed after the removed block (turn.rs:1296, 1303), so no unused-variable warning can appear.
- Regression test present and reproduces the defect (§2). `cargo test` reported 1111 green under `#![deny(warnings)]` (I cannot execute shell as reviewer; static inspection found nothing that could newly warn).

### 5. Security / diff-specific issues — none

Comment removal plus a sandboxed (tempdir) test; nothing attacker-reachable changed. Bookkeeping files are benign: `memverify.py` opens the DB strictly read-only (`mode=ro` URI); backlog item 56 (the user report this fix closes) removed; `stack.json` skill-stack swap; plan files are session notes.

## Findings list

| # | Severity | Location | Finding |
|---|----------|----------|---------|
| 1 | Minor (docs) | `src/agent/tests.rs:3881-3883` | Stale comment in existing test references the removed ">4 KiB invalidation" as the reason the recall cache is kept; reword as suggested above. |

No correctness, bug, security, or other constitution-compliance findings.
