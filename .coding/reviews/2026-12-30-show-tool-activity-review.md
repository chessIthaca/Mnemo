## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes (git diff HEAD + untracked) for plan 0e71e4f9 / backlog db489070 — the `show_tool_activity` setting (default off), a GUI-only render-time display filter consolidating and removing `show_memory_activity`. The implementation is correct and complete on every verified contract point; the single finding is a stale living-spec knowledge doc, no code impact.

## Verified — GUI-only contract (the core invariant)

- **No reducer/store suppression remains.** `agentEventReducer.ts` contains zero `showMemoryActivity` references; the `reduceToolCallStart` memory gate, the `reduceMemoryRecalled` gate, and the `applyAgentEvent` agent-stamping are all gone. Comments at :324, :493, and :1158 now document the always-enter behavior. `useAgentStore.ts` `registerAgents` no longer stamps; `setShowToolActivity` is a one-line global setter.
- **Render-time only.** `Conversation.tsx:107` is the sole `.transcript.map` renderer in the codebase (verified by search over all TSX) — the filter `showToolActivity || !isActivityEntry(entry) ? <Message key={i}/> : null` cannot affect any other consumer.
- **Model context untouched.** No `src/agent` / server-side files in the diff; the context echo is built server-side and unaffected.
- **Regression guard is real.** `useAgentStore.test.ts:1299-1313` sets `showToolActivity: false`, dispatches `memory_recalled`, and asserts the `memory` entry IS present in the store — this fails if anyone reintroduces reducer suppression. Exactly the right test for the changed path.

## Verified — consolidation completeness

- `show_memory_activity`: zero hits in source, tests, fixtures, README, or PLAN.md (all 38 hits are `.coding/` work records — plans/reviews/backlog — which are historical and fine).
- `showMemoryActivity`: the only non-archive hits are the intentional negative pin + comment in `Conversation.test.ts` (`expect(conversationSource).not.toContain("showMemoryActivity")`) — correct and desirable.
- `readShowMemoryActivity` / `LS_SHOW_MEMORY_ACTIVITY` / the `mh.showMemoryActivity` localStorage key: no remaining readers anywhere; the orphaned key is documented in the decision record.

## Verified — default-off + serde compatibility

- `UiConfig` (src/config/general.rs:322-330) uses `#[serde(default)]` and has **no `deny_unknown_fields`** — an old `config.toml` with `show_memory_activity` parses cleanly (unknown key ignored) and the stale key drops on next save (serialization emits only known fields).
- Default `false` (:349) pinned by `show_tool_activity_defaults_false` (:916-922, empty-config parse); `show_tool_activity_round_trips` (:953-965) covers parse → serialize → re-parse; `save_round_trips_all_sections` (:1000, :1038) covers the save/load disk round-trip.
- Wire shape: `GetSettingsUi.show_tool_activity` (:544), populate (:737), both wire-test literals (:1155, :1254) + the new assertion (:1197), and the contract fixture pair synced (`contract_fixtures.rs:231` ↔ `dto-get-settings.json:54`). `patch.rs` (:472-473, :582) and `settings_dto.rs` (:211, :514-515) apply paths renamed consistently.

## Verified — render-filter correctness

- `isActivityEntry` gates exactly `tool`/`memory`/`vision`/`skill`. The `TranscriptEntry` union (types.ts:310-371) has exactly 9 kinds — the other 5 (`user`/`assistant`/`error`/`steer`/`qa`) always render. No kind is missed; no phantom kind.
- React keys stable: the map runs over the full transcript with `key={i}` on the rendered branch; the `null` branch does not shift indices.
- Toggle-on identical to before: `showToolActivity || …` short-circuits true → every entry renders. Streaming text renders separately (:117), unaffected.

## Verified — project checks

- **Docs:** README.md / PLAN.md clean of the old key; module doc comments updated throughout (general.rs field doc, agentState.ts:409, useAgentStore.ts:248, Message.tsx, reducer comments). One stale knowledge doc — see L1.
- **Multi-platform:** pure Rust config + TS/TSX changes; no platform-specific code, paths, or shell syntax.
- **Warning-free:** green `cargo test` under `#![deny(warnings)]` at both crate roots (1930 passed, exit=0) is the proof; the diff removes dead code/imports cleanly with no new warning sources.

## Verified — test quality

- `Conversation.test.ts` (new): source pins pin the actual filter expression, the store selector, and the absence of the old gate — not tautological — plus pure `isActivityEntry` predicate tests over all 9 kinds (4 gated, 5 always-visible).
- The rewritten store test exercises the changed path (see above); Rust covers default + both round-trip paths + wire shape + fixture sync; `vitest.config.ts` include registration confirmed (773/773 across 54 files).

## Findings

### L1 — Stale living-spec doc: `.coding/knowledge/tool-call-echo-map.md` exception #1 describes the removed toggle as current behavior

`.coding/knowledge/tool-call-echo-map.md:47-57` ("The exceptions (by design — not suppression)", item 1) still says: *"Memory tools behind `show_memory_activity=false` (GUI chat only)… the ToolCard is suppressed from the chat transcript… **Default is `true`** (`src/config/general.rs:322-328` — the memory system should be visible out of the box)"* — citing the reducer suppression mechanism in `agentEventReducer.ts`.

This is now wrong on four counts: the toggle name (`show_memory_activity` → `show_tool_activity`), the scope (memory tools only → all tool/memory/vision/skill activity cards), the default (`true` → `false`), and the mechanism (reducer suppression → render-time filter in `Conversation.tsx` via `isActivityEntry`; the store keeps every entry). The same stale text is digested in `.coding/knowledge/spec/2026-12-29-tool-call-echo-map-every-llm-tool-call-echoes-no.md`.

Unlike plans/reviews (dated work records), this knowledge file is a **living spec** — the deliverable of the c8531c34 echo audit, describing current system behavior, and the pointer-first memory model directs future agents to "read the file for detail." As written it will actively misinform: an agent auditing echo behavior tomorrow would conclude memory cards are suppressed from the store by default-true config that no longer exists. The new DECISION record (2026-12-30) documents the new state, but the echo-map file contradicts it.

**Fix:** update exception #1 in `tool-call-echo-map.md` to describe the current contract (one `[ui] show_tool_activity` toggle, default `false`, gates all tool/memory/vision/skill cards at RENDER time in `Conversation.tsx` — the transcript store and the model's context echo are unaffected; the Output tab and console still log everything), and touch up the spec digest to match. Doc-only change; no code impact.
