## Verdict: FINDINGS (1 high, 2 low)

The code fix is correct on all three orphaning paths (mid-stream stop block, both bad-JSON arms, frontend sweep) with sound regression coverage and verified event ordering — no code defects found. The one high finding is bookkeeping, not code: `.coding/backlog.jsonl` loses user data (subject item deleted instead of closed, one item's status/note stripped, four done entries dropped) — it must be reconciled before the closing commit.

## Scope & method

Reviewed all uncommitted changes on `wt/agenticcoder` for bug-fix plan 24d2b6b9: full `git diff HEAD` (turn.rs, tests.rs, agentEventReducer.ts, useAgentStore.test.ts, backlog.jsonl) + untracked `.coding/` files (bug knowledge record, plan files). Verified against source: `DeltaAccumulator` (`src/provider/stream.rs`), the stream-forwarding select! loop, the tool-loop's existing not-run synthesis (turn.rs:1554/1875), `is_user_denial_tool_output` + its frontend mirror, `Message.tsx` `arePropsEqual`, `emit_child_finished` routing, `MockProvider` behavior, and the backlog.jsonl git history (clobber-repair commits 3b99a87 / 6e5cf25). Tests were reported green by the implementing agent (cargo 1696/0/1 warning-free, vitest 690/690); as a read-only reviewer I did not re-run them.

## Root cause confirmed ✓

The diagnosis is accurate on all three orphaning paths:

- **(a) Mid-stream stop block** (turn.rs ~1253): the `if stop_reason.is_some()` arm returns at line 1295 before the `acc.finalize()` at 1332 and the tool loop. Mid-stream `ToolCallStart`/`ToolCallArgDelta` are forwarded to the fan-in at 1085/1104 and fed into `acc` at 1037 (line 1005 — `acc` is re-created per loop iteration), so announced calls accumulate but never reach the tool loop's own not-run synthesis (1554/1875). Confirmed structural.
- **(b) Bad-JSON retry arm** (~1342): pushes `Role::Tool` error messages into history (1420-1434) but emitted no `AgentEvent::ToolResult` — the announced card spins while the turn continues, so no terminal event ever rescues it. Confirmed.
- **(c) Frontend**: `reduceFinished` swept nothing; `reduceError`'s final arm swept `kind === "vision"` only (the pre-change code in the diff). Confirmed.

## Fix correctness — verified ✓

**Backend stop block (turn.rs:1269-1289).** `acc.finalize()` returns every started accumulator entry — `ToolCallAccumulator::finalize` (stream.rs:33) returns `Some` whenever `id` OR `name` OR args are present, so a call announced via `ToolCallStart` (id+name set, args possibly truncated) always survives. The synthetic `ToolResult` sends precede the `Finished` send on the same channel with sequential `.await`s — ordering is guaranteed (single sender, mpsc FIFO). Checklist (f) ✓.

**Checklist (h) — `acc.finalize()` move safety.** `finalize(self)` consumes `acc`, but the stop block returns immediately after (line 1295); the `acc.finalize()` at 1332 is on the disjoint non-stop path (the stop block already returned). `acc` is re-created per iteration (line 1005), so the bad-JSON `continue` re-accumulates from scratch. No later use breaks. Rust's move checker + a green warning-free `cargo test` corroborate. ✓

**No-`Role::Tool` reasoning.** The partial assistant message records `tool_calls: vec![]` (line 1263); an orphan tool message would be provider-rejected on the next request (the class `validate_request_messages` catches). The stop block is uniform over `stop_reason.is_some()` — Interrupt/InterruptWithSteers/Cancel/Compact/Clear/Steer all fold into `stop_reason` before this block, so every stop-reason path reaching it gets the sweep, and none push history tool messages. The bad-JSON **cap** arm also returns before any history push (1375 precedes the sanitized-reinject block), so "the whole batch is discarded from history" holds — no orphan tool message there either. ✓

**Bad-JSON arms.** Retry arm emits per-call results after the model-facing history push; cap arm emits per-call results before the final `Error`. Both ordering-correct (the retry arm never reaches `Finished` on that path — `continue`). A later real `tool_result` still wins on the frontend (see below). ✓

**Frontend sweep.** `sweepRunningCards` finalizes tool calls with `result === null` (correctly handles merged cards carrying multiple parallel calls), running memory entries, and running vision entries. Wired into `reduceFinished`, `reduceError` (final arm only — the retrying arm is untouched, correct: the call may still be in flight), and `reduceChildFinished`. Verified `child_finished` is emitted tagged with the **child's** id (`emit_child_finished`, src-tauri/src/ipc/events.rs:954-965 → `agent_id: child_id`), so the sweep finalizes the finished child's own transcript, not the parent's concurrently-running cards. ✓

**Late-real-result overwrite.** `reduceToolResult` matches by `tool_call_id` scanning tool entries (agentEventReducer.ts:647) and memory entries by `entry.id` (623) — both overwrite the placeholder regardless of the sweep having run. Pinned by the second regression test. ✓

**Checklist (g) — `arePropsEqual`.** The tool comparator checks `calls` by reference; `sweepRunningCards` returns the same entry object for entries with no running call (no reference churn, no re-render) and a new `calls` array only for actually-swept entries (re-render fires exactly where needed). The memory comparator checks `running`/`success`, vision checks `description`/`success`/`running` — all changed by the sweep. Correct and efficient. ✓

**Checklist (e) — `bad_json_aborts_at_higher_cap` harness fix.** The overflow rationale is arithmetically sound: each bad-JSON iteration now emits ~9 fan-in events (ContextUsage, Phase-Waiting, pre-stream Phase, ToolCallStart, ArgDelta, Phase-Streaming, retrying-Error, **new** ToolResult), ×8 iterations > the 64-slot buffer — with the old post-hoc drain, `run_turn`'s `.send().await` deadlocks mid-turn. The concurrent-collector fix is sound: one collector owns `fanin_rx` (no double-drain — the old timeout-drain loop was removed, not kept alongside), `drop(fanin_tx)` after `run_turn` returns closes the last sender so the collector terminates, and the `saw_final_error` assertion is preserved over the *complete* event set (strictly stronger than the old 200 ms window). The `messages` assertions (7 tool messages) are unchanged. No lost assertion. ✓

**`mid_stream_steer_...` test mechanics.** The Notify-gate pattern is sound on the single-threaded `#[tokio::test]` runtime: during the test's `sleep`s the turn task is polled, the steer is folded into `stop_reason` while the stream is parked (a Steer folds but does not break the select! loop — 1222-1230 — so the gate release is required for the break, making the handshake deterministic); it also asserts the pre-existing soft-stop contract (`stop_reason == Steer`) plus ToolCallStart-before-result-before-Finished ordering. ✓

**Verified non-issues (checked, no finding):** the stop-block synthetic string `"interrupted: not run (turn stopped)"` matches `is_user_denial_tool_output`'s existing `"interrupted: not run"` substring, so mid-stream sweeps correctly do not extend the frontend doom streak or the backend MAX_RETRIES (they are UI-only and never land in history). `sweepRunningCards` is a no-op on clean finishes (every announced call already has a result by then), so the "(interrupted)" note can never appear on a legitimately-completed card. `capTranscript` is length-preserving under the map. The bad-JSON synthetic results' doom-streak interaction is F3 below.

## Findings

### F1 — HIGH (bookkeeping, not code): `.coding/backlog.jsonl` loses five records and regresses a sixth

The diff rewrites the backlog beyond this plan's scope:

- **Four closed entries deleted**: `41f39672`, `2980ca67`, `8863159f`, `aec6cc8a` (all `status:"done"` with close-out notes) are dropped entirely.
- **`931ebe06` regressed**: `status:"cant_resolve"` + note `"plan loop did not close (workflow: Reviewing) — rolled back to checkpoint"` → `status:"pending"`, note `null`.
- **The subject item `63cbc20f` is deleted** rather than closed.

This is the exact clobber class repaired twice before (commits `3b99a87` "repair steering-time rewrite of backlog.jsonl" and `6e5cf25` "repair second steering-time backlog clobber"): done entries and the cant_resolve status live only in `HEAD`'s copy. backlog.jsonl travels with git (union merge driver), so committing this loses the history for every instance. **Required before commit:**
1. Restore from `HEAD`: the four done entries and `931ebe06`'s `cant_resolve` status + note (the new items `a39830cf`, `4bd98d68`, `c6bbbddd` and the kept `c7383c50`/`24e1c98e` are legitimate — keep them).
2. Close `63cbc20f` as `status:"done"` with a close-out note per the repo convention ("Item closed as done" pattern, e.g. commits 9ca60dc/a7bed2a) instead of silently deleting it. Dropping its embedded base64 screenshot is fine and even desirable (that's what new item a39830cf complains about) — replace `images` with `[]` — but the record itself should close, not vanish. If the deletion was a deliberate user action, split it into its own commit with a message saying so rather than burying it in this fix.

### F2 — LOW: the bad-JSON **cap** arm's new per-call ToolResults are emitted but never asserted

`bad_json_tool_call_emits_tool_result_event` pins the **retry** arm only (one bad pass, then a clean pass). `bad_json_aborts_at_higher_cap` drives the **cap** arm (turn.rs:1349-1365) and now collects the full event vector via the new collector, but asserts only `saw_final_error` and the message count — a regression that deletes the cap-arm `for tc in &tool_calls` send loop would still pass the entire suite. The two arms are textual near-duplicates today, which is exactly why the weaker one should be pinned. Suggested: in `bad_json_aborts_at_higher_cap`, after `let events = collector.await...`, add `assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolResult { tool_call_id, .. } if tool_call_id == "call_1")), "the cap arm must end the announced card (backlog 63cbc20f)");` — one line, no harness change.

### F3 — LOW: synthetic bad-JSON results are the first failed ToolResults that count toward the frontend doom streak but are invisible to the backend's exclusion list

`isUserDenialToolOutput`'s doc contract is parity with the backend `is_user_denial_tool_output` ("Mirrors the backend's … EXACTLY — same substrings"). The mid-stream stop-block string deliberately matches (`"interrupted: not run"`); the new bad-JSON strings (`"arguments malformed or truncated — not run"`, `"…not run; retrying"`) deliberately do not — so each retry extends `consecutiveToolErrors` (and double-counts with the same retry's `retrying:true` error event, which already extends the streak). The backend never sees these results (UI-only, never in history), so its MAX_RETRIES exclusion for bad JSON is intact — the parity claim is now subtly stale in spirit: the frontend list is applied to a stream containing UI-only synthetic results the backend list cannot see. Observable difference is narrow (the streak reaches DOOM_ERROR_STREAK earlier during a bad-JSON stretch; any successful retry resets it identically, and the 8-strike abort playing doom is defensible). Recommend one of: add `"arguments malformed or truncated"` to `isUserDenialToolOutput` (bad JSON is a recoverable model hiccup, matching the backend's MAX_RETRIES stance), or add one line to the emit-site comment stating these intentionally count toward the streak. Either closes the ambiguity.

## Checklist verdicts

| Check | Verdict |
|---|---|
| (a) Regression tests exercise the changed paths | ✓ — stop block pinned by `mid_stream_steer_emits_not_run_result_for_announced_tool_call` (asserts the synthetic result AND its position before `Finished` AND the carried Steer); bad-JSON retry arm pinned by `bad_json_tool_call_emits_tool_result_event`; frontend sweep pinned by 4 store-dispatch tests (finished sweep, late-result overwrite, final-vs-retrying error, memory entry). Gap: the **cap** arm (F2). Root cause documented (BUG record at `.coding/knowledge/bug/2026-08-30-tool-cards-stuck-running-after-mid-stream-steer.md`, content matches the shipped fix). Regression test name recorded in `.coding/plans/24d2b6b9.md` (`## Regression test` section). |
| (b) Documentation sync | ✓ — no new events/config/user-facing surface; PLAN.md documents the event enum shape only (unchanged) and never described the stop block, so nothing is stale. The new helper carries a doc comment and each emission site has inline rationale; the BUG knowledge record carries the invariant. No README/PLAN.md update required. |
| (c) Multi-platform neutrality | ✓ — pure async Rust + TS, no paths/OS APIs/shell syntax anywhere in the diff. |
| (d) `#[allow(...)]` / suppressed warnings | ✓ — none added; reported warning-free build under `deny(warnings)`. |
| (e) Deadlock fix soundness | ✓ — see "Fix correctness" above: no double-drain (old drain loop removed), no lost assertion (`saw_final_error` preserved over a strictly larger event set), overflow arithmetic confirmed against the per-iteration event count. |
| (f) Synthetic results strictly before `Finished` | ✓ — stop block sends results (1279-1289) then `Finished` (1290); cap arm sends results then the final `Error`; retry arm `continue`s (no `Finished` on that path). Sequential `.await` sends on one channel make ordering airtight. |
| (g) `arePropsEqual` under sweeps | ✓ — reference equality on `calls` remains correct; the sweep preserves references for untouched entries and produces new arrays only for swept ones. |
| (h) `acc.finalize()` move safety | ✓ — stop block returns immediately after; disjoint from the 1332 finalize; `acc` re-created per iteration. |

## Summary

The code change is correct, minimal, well-precedented (it mirrors the tool loop's own synthesis at 1554/1875 and the reduceError vision precedent), and properly regression-pinned on both sides of the IPC boundary. Fix F1 before committing (restore the lost backlog records, close the subject item properly); F2 and F3 are small hardening steps — fix or justify. F1 is the only blocker.