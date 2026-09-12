# Review — CodeGraph watcher, stop-flushes-pending, steer history (plan 506f75c5)

Scope reviewed: ALL uncommitted changes (`git status` / `git diff HEAD`): `src/codegraph/watcher.rs` (new), `src/codegraph/mod.rs`, `src-tauri/src/main.rs`, `src/agent/loop_impl.rs`, `src/agent/turn.rs`, `src/agent/tests.rs`, `src/agent/prompt.rs`, `src/runtime/agent.rs`, `frontend/src/components/layout/InputBar.tsx`, `Cargo.toml`, `Cargo.lock`.

I read the actual code at every site named in the review focus, plus the supporting context (`walk.rs`, `search.rs::should_search`, `channels.rs::AgentCommand`, `mod.rs::index`, the `PausingMockProvider` test harness).

## Verdict

The three work items are correct and well-tested. The steer-accumulation core (the riskiest change) is sound at every site. Findings are all Low — no High/Medium correctness, security, or constitution issues.

---

## Findings

### Low 1 — Watcher defer is a single sleep, then indexes regardless (possible concurrent passes)
**File:** `src/codegraph/watcher.rs:149-156`

When `graph.is_indexing()` is true, the loop sleeps one `debounce` and then calls `index()` *unconditionally* — it does not re-check the flag or loop until the other pass finishes. If the still-running pass (startup or manual refresh) outlasts one debounce period, two `index()` passes run concurrently.

This is **not a correctness bug**: both passes are full content-hash-gated sweeps and the `store: Mutex<Store>` serializes the per-file writes and the final `prune_missing`/`rebuild_edges`, so the end state converges. But the doc comment on `spawn` ("If the graph is already indexing … the pass is deferred briefly and retried — a burst is never lost, just delayed") slightly overstates the mechanism: there is no retry loop, just one fixed deferral. Consider either re-checking `is_indexing()` in a short wait-loop, or softening the comment to say the pass is deferred once and may run concurrently (harmlessly, via the store mutex).

### Low 2 — Tool-batch drain inlines steer accumulation instead of calling `StopReason::fold`
**File:** `src/agent/turn.rs:999-1013`

The tool-batch `try_recv` drain routes `Interrupt` through `StopReason::fold` (line 988) but hand-rolls the `Suggestion`/`Prompt` accumulation with a local `match &mut stop_reason` (lines 1001–1013). This is functionally identical to `fold`'s steer arm (`Some(Steer) | Some(InterruptWithSteers) => push`, `None => Steer(vec![..])`, `_ => keep`), but it duplicates the logic rather than delegating — and the inline version's `_ => {}` arm silently keeps a `Cancel`/`Compact`/`Clear` the same way `fold` does, so behavior matches. The mid-stream select (line 773) and the approval re-injection (line 1099) both call `fold` for the same operation. Delegating here too (`StopReason::fold(&mut stop_reason, cmd)` for the steer/prompt arms) would remove the only place the accumulation rule is written twice, so a future change to the rule can't drift.

### Low 3 — `fold` drops buffered steers on `Compact`/`Clear` (matches contract, but worth noting)
**File:** `src/agent/loop_impl.rs:285-287`

`fold`'s `Cancel`/`Compact`/`Clear` arms unconditionally overwrite `current`, discarding any already-buffered `Steer(..)`. For `Cancel` this is correct (the agent task exits; steers are moot). For `Compact`/`Clear`, a steer queued in the same batch window is silently dropped. This matches the documented contract (the doc comment only promises preservation for `Interrupt`, lines 258–264) and preserves the pre-existing "stronger signal wins" semantic, and the window (steer + `/compact` drained in the same `try_recv` batch) is extremely narrow. Flagging only so the behavior is a conscious choice, not an accident — if pending commands should survive a `/compact` too, these arms would need the same `InterruptWithSteers`-style preservation. No change required if the current semantic is intended.

---

## Focus-area verification (all clean)

**Watcher correctness**
- *Debounce coalesces to one pass:* the inner loop (watcher.rs:132-144) sleeps `debounce`, drains with `try_recv`, resets on any event, and breaks only on a quiet period — a burst of N events yields exactly ONE `index()` call. ✓
- *Filter parity with `walk_project`:* `is_indexable_path` (watcher.rs:36-52) applies `is_ignored_component` per component + extension gating via `Lang::from_extension`, matching `walk.rs`. The omitted 1 MB guard is documented (lines 31-35) and harmless: `index()` itself re-runs `walk_project` → `should_search` (mod.rs:175), which re-applies the size guard, so a >1 MB event only triggers a cheap no-op pass. ✓
- *Handle kept alive / drop stops watch:* `GraphWatcher` holds `_watcher` + `_task`; stored as `_graph_watcher` on `Brain` (main.rs:616-621, 1059). Dropping `Brain` drops the watcher, the channel closes, and `debounce_loop` exits on `recv() = None` (watcher.rs:124-126). ✓
- *Sync callback → tokio bridge:* `unbounded_channel` + synchronous `send` in the `recommended_watcher` closure (watcher.rs:96-100) — no `await` in the sync callback. Sound. ✓
- *Root escape:* `strip_prefix` guard (watcher.rs:37-40) rejects outside-root events. ✓
- *Cargo consistency:* `notify = "8"` in root `Cargo.toml`; `Cargo.lock` shows `notify 8.2.0` under `myharness` deps with `inotify`/`kqueue`/`fsevent-sys`/`windows-sys` backends. ✓

**Steer/Interrupt correctness**
- Every mid-turn command is now accumulated (`Steer(Vec)`) or hard-stop-with-preservation (`InterruptWithSteers`) at all four sites: provider-request select (turn.rs:520-534, drains `buffered_steers` into `InterruptWithSteers`), mid-stream select (turn.rs:754-774, via `fold`), tool-batch drain (turn.rs:983-1014), stop_signal merge (turn.rs:1070-1085, approval-gate Interrupt routed through `fold` instead of clobbering). ✓
- `InterruptWithSteers` is treated as a hard stop at all three `matches!` sites (turn.rs:577-581 early return, 1016-1022 tool-batch break, 1072-1077 `hard_stop`) AND runs the carried steers in the consumer (agent.rs:100-123, `Steer(steers) | InterruptWithSteers(steers)` pushes every steer + `SuggestionInjected` each + follow-up turn). No command double-run or lost. ✓
- Vec-form test updates (tests.rs:452, 2880) are correct (`vec!["..".into()]`). ✓

**Regression-test quality**
- `mid_turn_multiple_steers_are_never_dropped` (agent.rs:1327): asserts the exact `injected` vec of all 3 steers in arrival order + `finished_count == 2`. On the old first-steer-wins code, `injected` would be `["first queued"]` and the assert_eq fails. Tight. ✓
- `stop_with_queued_steers_runs_them_immediately` (agent.rs:1443): asserts both queued steers injected, 2 Finished, and `!saw_exited`. On the old Interrupt-clobbers-steer code, `injected` is empty and `finished_count == 1` — fails. Tight. ✓

**History fix (InputBar.tsx)**
- `pushPrompt(input)` is correctly placed in the steer branch (line 263), after the empty-input guard (247) and before `addSteer`/`sendSuggestion`, so pending commands reach up/down history and dedupe via `pushPrompt`. Slash commands still return early (275-280) and are not recorded — consistent with the corrected comment (250-252). The regular-prompt path (284) is untouched. ✓

**Constitution**
- `_graph_watcher` uses the underscore-prefix Drop-field idiom, not `#[allow]` (main.rs:616-621, with an explanatory doc comment). ✓
- Doc comments on all pub items: `GraphWatcher`, `GraphWatcher::spawn`, both new `StopReason` variants, `StopReason::fold`. ✓
- Regression tests added for both defects (multi-steer drop; Interrupt clobber). ✓
- No `#[allow(...)]` added anywhere in the diff. ✓

(Note: I am read-only and did not run `cargo test`; the green-build/warning-free verification under `#![deny(warnings)]` must be confirmed by the main agent per the closing sequence.)
