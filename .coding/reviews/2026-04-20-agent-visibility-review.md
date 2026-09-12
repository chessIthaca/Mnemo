# Review — Agent visibility (effective-model display + memory-usage notifications)

**Branch:** fix/agent-visibility · **Date:** 2026-04-20 · **Scope:** all uncommitted changes (`git diff HEAD` + 2 untracked frontend files + `.coding/plans/*`).

Read-only review; builds/tests not run by the reviewer (the implementing agent's claimed results — cargo 1062 + src-tauri 67 + vitest 228 + clean `npm run build` — were taken as given; nothing in the diff looks non-compiling: all imports/uses in the new Rust tests resolve, all new TS identifiers exist).

## Findings

### 1. BUG (correctness, moderate) — `MemoryRecalled` fires on cache-reuse iterations, contradicting its own contract → transcript spam

**File:** `src/agent/turn.rs:362-422` (emit at 395-412)

The plan and the in-code comment both promise the event fires **only on a FRESH non-empty recall** ("the cache-reuse branch … emit[s] nothing", plan focus point (a); comment at turn.rs:396-401: "Emitted only for a non-empty FRESH recall"). The code does not implement that:

```rust
let results = if last_recalled_user_query.as_deref() == Some(q) {
    if let Some(cached) = &last_recall_results { cached.clone() }   // ← cache reuse
    else { store.recall(...) }                                       // fresh
} else { store.recall(...) };                                        // fresh
if !results.is_empty() {
    // ← emit is HERE, outside the branch distinction (turn.rs:402-412)
    send(AgentEvent::MemoryRecalled { hits: results.len(), ... });
    Some(prompt::format_recall_context(&results))
}
```

The per-turn recall cache is invalidated only on summarize (turn.rs:324-325) or a tool result > 4096 bytes (turn.rs:1291-1294). So in a typical multi-iteration turn (several tool calls, small results, unchanged user query), **every loop iteration re-emits an identical `MemoryRecalled`** — one extra event per tool round-trip.

Frontend impact: `reduceMemoryRecalled` (`frontend/src/hooks/agentEventReducer.ts:719-736`) pushes one `memory` transcript entry per event with no dedupe, and `show_memory_activity` now defaults to **true** — an N-iteration turn renders N identical "auto-recall — 3 hit(s) — top: …" entries, which is exactly the transcript spam this feature's gating was meant to avoid.

Test gap: `auto_recall_emits_memory_recalled_event` (`src/agent/tests.rs:3880-3956`) asserts `recalled.len() == 1`, but its provider is `MockProvider::single` → the turn runs **exactly one loop iteration**, so the test passes under both the intended (fresh-only) and actual (per-iteration) behavior. It does not pin the contract its own comment states ("exactly one MemoryRecalled event per fresh non-empty recall").

**Fix:** gate the emit on freshness — move the `send` into the two fresh `Ok(r)` arms (or capture a `fresh: bool` from the branch and check it alongside `!results.is_empty()`), update the comment, and extend the regression test to a two-iteration turn (round 1 = a small-output tool call, round 2 = Stop) asserting still exactly one event.

### 2. OBSERVATION (not a defect) — the default flip reaches fresh profiles only

`useAgentStore.ts:530` initializes `showMemoryActivity` from `readShowMemoryActivity()` (`appearance.ts:278-281`), which defaults to **false** when the `mh.showMemoryActivity` localStorage key is absent. `App.tsx:246-248` then stamps the backend value only when that key is unset. Prior versions' first-run hydration called `setShowMemoryActivity(false)` (the old backend default), and the setter persists to localStorage (`useAgentStore.ts:714-715`) — so existing installs carry `false` and the hydration guard now skips them. Net effect: "memory activity visible by default" applies to new installs (or cleared storage); existing users keep their implicit opt-out. This is arguably the right migration stance (don't override a visible preference), and no code change is required — flagged so the plan author can confirm it was a deliberate choice rather than an oversight.

## Verified clean (focus points a–g)

- **(a) ModelChanged path** — `turn.rs:163-188`: `prev_resolved` captured before, `now_resolved` after; the tokio `workflow` guard is scoped to the inner block and dropped before the `fanin_tx.send(...).await` (no lock held across the await). `resolve_turn_provider` sets `resolved_model` on every path (`loop_impl.rs:635/644/655/665`), so the `now != prev` comparison is sound, including the forced-model and build-failure (deleted endpoint → `None` → default) edges. `None` maps to `default_provider.model()` (the turn-start snapshot, `turn.rs:62-66`) — truthful. Fires only on change: the negative test (`no_model_override_no_model_changed_events`) pins zero events; the positive test pins the exact `["skill-model", "mock"]` override→revert sequence (consistent with `MockProvider::model() == "mock"`, `tests.rs:60-62`). No interaction with the IPC `set_model` path (it clears `resolved_model` to `None`, so the next turn's first resolution can't double-emit).
- **(b) Wire parity** — `SerializableAgentEvent` has `#[serde(tag = "kind", rename_all = "snake_case")]` (`channels.rs:283`) → `"memory_recalled"`. Rust contract fixture (`tests/contract_fixtures.rs:211-216`: hits 3 / "merge instructions") ⇄ `frontend/src/lib/ipc-fixtures/event-memory-recalled.json` (identical) ⇄ `types.ts:146` (`hits: number; top_title: string | null`) ⇄ `channels.rs:202-212/338` (`usize` ⇄ number, `Option<String>` ⇄ string|null). Round-trip test `memory_recalled_roundtrips_json` present (`channels.rs:822-843`) and asserts no oneshot senders.
- **(c) Default-flip coverage** — Rust default (`general.rs:293`) + renamed/updated test (`general.rs:711-717`); both wire-shape literals (`settings.rs:1387, 1469`); both golden fixtures (`contract_fixtures.rs:216`, `dto-get-settings.json`). No stale `false`-default assertions remain (`general.rs:765` is an explicit non-default literal inside a save/load round-trip test — correct as-is; `settings.rs:606` reads from config). Frontend DTO/reducer/tests all aligned.
- **(d) reduceMemoryRecalled** — pushed already-finalized (`running: false`) and `name: "auto-recall"` ∉ `MEMORY_TOOLS`, so `reduceToolResult`'s finalization loop (`agentEventReducer.ts:360` requires `entry.running && MEMORY_TOOLS.has(entry.name)`) cannot match it — double protection. Gated by `agent.showMemoryActivity`; suppressed-when-false case tested (`useAgentStore.test.ts:766-772`); both `resetStore` fns seed `showMemoryActivity: false` (`useAgentStore.test.ts:37`, `ipc-contract.test.ts:103`) preventing cross-test leaks; auto-created agents are stamped with the global flag (`agentEventReducer.ts:845-848`), matching the `registerAgents` (`useAgentStore.ts:573-575`) and `setShowMemoryActivity` re-stamp (`714-725`) paths.
- **(e) parseMemoryEntry pin** — `agentEventReducer.memory.test.ts` reproduces the real `memory_recall` output from `src/tool/memory/mod.rs:176-187` byte-for-byte (`"N memories matched:\n\n[tier] title (score: X.XX, strength: Y.YY)\n  content\n\n"`); traced the regexes (`/\[(\w+)\]\s+(.+?)\s+\(score:/` and the following indented-line match) against the pinned sample — tier/title/snippet all extract correctly; the empty-recall case yields `undefined` tier/title. Registered in `vitest.config.ts:23`.
- **(f) Constitution** — doc comments on all new public items (both `MemoryRecalled` variants, `reduceMemoryRecalled`, exported `parseMemoryEntry`); no `#[allow]` added; regression tests for each defect (3 Rust + frontend suites). Warning-free claim accepted per the reported green runs under `#![deny(warnings)]`.
- **(g)** `.coding/plans/stack.json` + new plan file are bookkeeping only. CRLF→LF warnings are the repo's `.gitattributes eol=lf` one-time normalization — not a defect, as noted.

## Summary

One real bug (finding 1: `MemoryRecalled` emitted per loop iteration instead of per fresh recall — implementation contradicts its documented contract, the regression test is too weak to catch it, and the now-default-on flag makes the resulting transcript spam user-visible) plus one deliberate-behavior confirmation (finding 2). Everything else — ModelChanged lock discipline and semantics, four-way wire parity, parser pin, reducer gating/stamping, default-flip coverage, constitution compliance — is clean.
