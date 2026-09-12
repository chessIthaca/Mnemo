## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of the round-1 L1 fix for plan 0e71e4f9 / backlog db489070 (`show_tool_activity`). **The doc fix is correct and complete on every mandated count, commit a65862d is exactly the round-1-verified change plus the doc fix (zero code changes since round 1), and every round-1 verification point still stands on the committed tree.** The single finding is workflow bookkeeping, not a defect in the reviewed change: backlog item db489070 is `[pending]` with a stale mid-work note in the committed tree although its plan is complete and shipped (see Findings).

## 1. The L1 fix — verified correct and complete

**`.coding/knowledge/tool-call-echo-map.md` exception #1 (lines 49-64)** — every claim checked against the committed code:

- **Name + default:** one `[ui] `show_tool_activity` toggle, default **false** — verified: field + doc comment at `src/config/general.rs:322-329`, `show_tool_activity: false` in `Default` at `:349`. The doc's citation `general.rs:322-330` covers the field block. ✓
- **Scope:** ALL agent-activity cards (tool calls, memory reads/writes, vision image-parsing, skill announcements) — matches `isActivityEntry` (agentState.ts), which gates exactly `tool`/`memory`/`vision`/`skill`; the doc's `MEMORY_TOOLS` member list {write, search, consolidate, update, supersede, delete} matches `agentEventReducer.ts:327-336` exactly. ✓
- **Mechanism:** render-time — `Conversation.tsx:107` skips entries via `isActivityEntry` — verified: `:107-110` is `state.transcript.map((entry, i) => showToolActivity || !isActivityEntry(entry) ? <Message/> : null)`. ✓
- **Store unaffected:** the transcript STORE always contains every entry — verified by direct read: `reduceToolCallStart` (:491-515) always pushes memory entries (gate gone), `reduceMemoryRecalled` pushes unconditionally. ✓
- **Model context unaffected:** no `src/agent` files in a65862d (stat shows only `src/config/*`, `src-tauri/src/ipc/*`, and frontend files). ✓
- **Other surfaces:** Output tab still logs every tool result; console always prints `-> {name}` — round-1 verified, those paths untouched by the commit. ✓
- **Supersede note:** lines 62-64 record the old memory-only `show_memory_activity` reducer suppression (default `true`) as removed 2026-12-30. ✓

**Digest file** (`.coding/knowledge/spec/2026-12-29-tool-call-echo-map-every-llm-tool-call-echoes-no.md`): the exception-(1) phrase now reads "agent-activity cards … hidden from GUI chat at RENDER time when [ui] show_tool_activity=false — default false …; the transcript store keeps every entry and the model context echo is unaffected; Output tab + console still show them" — accurate on name, scope, default, mechanism, and both unaffected surfaces, with the supersede note intact. ✓

**No stale `show_memory_activity`-as-current text remains in either file.** A literal walk over `.coding/knowledge/**` finds exactly 4 matches in 3 files: the two supersede notes (correctly describing the old toggle as removed), the fixed digest phrase, and the new DECISION record (which documents the removal). No other living-spec knowledge file describes the old toggle as current.

**Citation nuances checked, acceptable (not findings):** the rewritten exception #1 cites `agentEventReducer.ts:329` for `MEMORY_TOOLS` — the set definition spans :327-336, so the citation lands inside the cited construct (on a set member), and the doc's member list is exact. `Conversation.tsx:107` is the map statement whose next line carries the filter — the same citation round 1 itself used.

## 2. No new inaccuracies introduced

- The a65862d diff for `tool-call-echo-map.md` contains **exactly one hunk** (`@@ -46,15 +46,22 @@`) — exception #1 only. Exceptions 2-4, the echo-chain section, and the rarity analysis are byte-identical to the pre-fix text.
- The digest diff is a **single line** (the exception-(1) phrase); the rest of the paragraph is untouched.
- Untouched sections remain substantively accurate: exception 2 (auto-delegation — `codegraph.rs`/`search.rs` untouched by this commit), exception 3 (sub-agents — spawn paths untouched), exception 4, and the rarity analysis (merge rules + auto-delegation behavior confirmed unchanged in `reduceToolCallStart` :516-552).
- Pre-existing line-pointer drift in the *untouched* echo-chain citations (`:486` → `reduceToolCallStart` now at :484; `:545` → `neverGroups` now at :541-545) — caused by the round-1-approved code change itself; both citations still land inside their cited constructs and the described behavior is verified true. Correctly left alone by the fix (a doc-fix-only commit must not expand scope).

## 3. Commit spot-check (a65862d) — clean

- a65862d is HEAD; the working tree is clean (`git diff HEAD` and `git status` both empty) — the reported green test evidence (root cargo exit=0, src-tauri cargo exit=0, vitest 773/773 across 54 files, npm run build green) was run against exactly this tree.
- 29 files: 21 code/test/fixture files + 8 `.coding/` artifacts. Every code file matches round-1's verified change set (App.tsx hydration, Conversation.tsx filter + new Conversation.test.ts, Message.tsx comment updates, ChatSection/types renames, agentEventReducer gate removals, agentState `isActivityEntry`, appearance.ts LS-key removal, useAgentStore flag + test rewrites, ipc-contract/tauri/fixtures/vitest.config, and the Rust general.rs/patch.rs/settings_dto.rs/settings.rs/contract_fixtures.rs + dto-get-settings.json).
- Post-round-1 content is **only**: the doc fix (the two knowledge files), the round-1 report file itself, and plan/knowledge/backlog bookkeeping (including the 7251fc05 research-plan side-car record from the same branch — no code impact). **No code changes since round 1.** ✓

## 4. Round-1 verification points — all stand, no regressions

- **GUI-only contract:** zero `showMemoryActivity` references in `agentEventReducer.ts` (gates + agent stamping gone; memory entries always enter the store — re-verified by direct read); `Conversation.tsx:107-110` remains the sole transcript renderer carrying the render-time filter; no `src/agent` files in the commit.
- **Consolidation completeness (re-run on the committed tree):** `showMemoryActivity|show_memory_activity` over `frontend/src/**` → only the intentional negative pin + its comment in `Conversation.test.ts:39,41`; over all 257 `.rs` files → zero matches.
- **Default-off + serde:** `general.rs` field (:322-329) + `Default` `false` (:349) re-verified by read; round-1's test inventory (defaults_false, round_trips, save_round_trips, wire assertions, synced fixture) is exactly what the commit ships.
- **Regression guard:** the rewritten store test asserts the `memory` entry IS present in the store with `showToolActivity: false` — unchanged since round 1, still the right test for the changed path.
- **Multi-platform / docs:** pure config + TS/TSX, no platform-specific code; README/PLAN.md clean (round-1; nothing in the commit touches them).

## Findings

### L1 — Backlog item db489070 is left `[pending]` with a stale steer note in the committed tree

`backlog.jsonl` as committed in a65862d (and confirmed live via the backlog list) shows db489070 as `status: pending`, note `"steered by the user, returned to queue"`, `plan_id: 0e71e4f9` — although the plan is complete (all steps [x], closing sequence underway), the work is committed (a65862d), and both review rounds are done. Plan step 4(3) explicitly required marking the item done with a note pointing at the commit; that state is reflected nowhere (the tree is clean, so there is no uncommitted flip pending either). Compare the previous item on this branch, 569b5922, which is correctly `[done]` with its mid-work steer note cleared.

Impact: per the status ⇔ plan-lifecycle semantics (45dcf577), a `[pending]` item with no in-flight plan is dispatchable — a future run-all would re-dispatch this already-shipped item and re-do the work. The stale note also misdescribes the item's actual state to every future reader.

Fix (bookkeeping only — fold into the final commit with this report): mark db489070 done with a note citing the commit (e.g. "done — plan 0e71e4f9, commit a65862d; round-1 FINDINGS 0H/1L fixed, round-2 verified"), via `backlog_status` or a direct one-line `backlog.jsonl` edit if the tool is filtered in Reviewing. If the user genuinely re-queued this item for further work, skip the flip and record that justification instead.
