## Verdict: FINDINGS (2 high, 2 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 7369d7f3 "Auto-compact between run-all items (auto_compact_on_plan_complete)".

**Summary:** The core design is implemented correctly and soundly — the spawn-based deadlock avoidance, subscribe-before-send watch-counter semantics, pairing-guarded main-agent-only forwarder increment, and every-failure-path-proceeds dispatch are all verified good, and the tests genuinely pin the contract. However, the frontend **production build is broken** (store initializer missing the new required `compacting` field — vitest green does not catch it, `tsc` does), and the now-minutes-long between-items window lets user stop/steer actions be silently overridden by the compact task's unconditional dispatch.

---

## Verified sound (design points 1–11)

- **Deadlock avoidance (sound).** The gate spawns `compact_then_dispatch_next` (never awaited inline — pinned by the `auto_compact_gate_is_spawned_and_run_all_only` source-contract test). Subscribe-before-send is correct: `before` is recorded synchronously right after `subscribe()` with no await in between, and the increment can only fire after the `Compact` command is processed, so the own-compaction increment is always past `before` — no missed wakeup; no stale-permit false wake (earlier increments are baked into `before`). The `borrow() > before` re-check + `changed()` loop is correct tokio watch usage. Critically, `compact_context` (src/runtime/agent.rs:883) emits only `CompactStarted` → (`Compacted` | `Error { retrying: true }`) — no `Started`/`Finished` — so compaction cannot re-trigger turn resolution, and the spawned task waiting on the forwarder's increment has no cycle.
- **Failure handling (sound).** All five paths — timeout (`AUTO_COMPACT_WAIT` 600s), failed compaction (the `Error` pairing increments, so the wait resolves immediately instead of burning the timeout), no main agent, refused send, closed channel — log and proceed to `run_all_dispatch_next`. The `compacting` flag is cleared on every path (no early return between set and clear).
- **Run-all-only gating (sound).** The gate sits in the run-all branch after the `stopped` check and before the single-dispatch/auto-feed path (which follows the `return`); interactive completions never reach it. Setting-off path is behaviorally identical to before (direct dispatch).
- **Forwarder increment (sound).** Pairing-guarded by `compact_in_flight` (`CompactStarted` inserts; `Compacted`/`Error` remove-and-increment; `Exited` cleans up), main-agent-only via `main_agent_id() == Some(agent_id)`. Ordinary turn errors are never in the set → no increment. A retrying compaction error correctly pairs and resolves the wait.
- **Settings plumbing (sound).** Mirrors `enable_browser_inspection` end-to-end: `GeneralSection` field + `Default` + defaults/round-trip tests; `SettingsSaveDto` + `validate_and_apply_settings_patch` arm + patch test; `GetSettingsGeneral` + `from_config` + wire-shape test literals + both fixtures; `tauri.ts` get/patch; `AdvancedSection` state/snap/load/dirty/save/checkbox. Defaults false at every layer.
- **Progress line (sound).** `RunAllProgress.compacting` set before send, cleared before dispatch, `backlog-changed` emitted on both flips; rendered only while a run is active; `BacklogView` renders " · compacting…" after done/total; `ipc-contract.test.ts` asserts the field; both fixture JSONs updated.
- **Tests pin the contract (sound).** `fn_body` composes its needle as `fn {name}(` — test names and string literals cannot match it, so the three source-contract tests slice the real functions, not themselves. The three behavioral wait tests are deterministic (increment-before-wait resolves via the borrow check; timeout asserts elapsed ≥ deadline; dropped sender resolves `changed()` → `Err` immediately). Config tests pin default/round-trip/patch.
- **Platform neutrality / security / quality (sound).** Tokio watch + atomics, `eprintln` matching file style, no Windows assumptions in Rust or TS. No new attack surface — the setting only triggers compaction of the main agent. Doc comments on all public items (and the private helpers); no dead code; Rust side warning-free by inspection.

---

## Findings

### HIGH-1: Frontend type-check/build broken — store initializer missing the new required `compacting` field

`RunAllProgress` (frontend/src/lib/types.ts:46) gained a **required** `compacting: boolean`, but the zustand store's initial state was not updated:

- frontend/src/hooks/useAgentStore.ts:677 — `runAll: { active: false, done: 0, total: 0 },`

The initializer is contextually typed (`create<AppState>` at useAgentStore.ts:595; `runAll: RunAllProgress` at :360), so this is a hard `tsc` error (TS2741: property 'compacting' missing). `frontend/package.json:9` — `"build": "tsc && vite build"` — so **the production build fails**. The claimed "frontend vitest green: 971 passed" does not cover this: vitest transpiles without type-checking; the plan's own verification step (`npx tsc --noEmit`) would have caught it. Runtime impact is benign (undefined is falsy, so "compacting…" never shows pre-hydration), but the build is broken.

**Fix:** `runAll: { active: false, done: 0, total: 0, compacting: false },` — then run `npx tsc --noEmit` in frontend/ to confirm.

### HIGH-2: User stop/halt during the between-items compaction window is silently overridden — `compact_then_dispatch_next` dispatches unconditionally

`compact_then_dispatch_next` (src-tauri/src/ipc/run_all.rs:1135) calls `run_all_dispatch_next` after the wait without re-checking the run state. Pre-change, the stopped-check → dispatch sequence was synchronous (a microsecond window inside `on_main_turn_resolved`); this change stretches that window to the compaction duration (up to `AUTO_COMPACT_WAIT` = 600s), during which user actions land and are then overridden:

- **Stop overridden:** `backlog_stop_all` (src-tauri/src/ipc/backlog_cmds.rs:443) only sets `r.stop = true`; its only readers are in `on_main_turn_resolved` (run_all.rs:1450/1551). A stop clicked while the progress line shows "compacting…" is ignored — the next item is dispatched and fully worked (plan, edits, commits), and the stop only takes effect after that extra item's turn resolves. This violates `backlog_stop_all`'s documented contract ("Stop the Run-All loop after the current in-flight item resolves" — the in-flight item *has* resolved).
- **Steer → orphan dispatch (worst case):** a steer on the main agent during the window finds `running == false` (compaction emits no `Started`), so `send_suggestion` records no intervention latch, but still calls `halt_run_all` unconditionally (src-tauri/src/ipc/agent.rs:280) → `end_run` (run_all = None). The compact task then dispatches the next item with **no active run**: `run_all_dispatch_next` has no run-active check, and at that turn's resolution the single-dispatch path finds `single_in_flight` empty — nothing ever resolves the item. It stays `Pending` while its work runs, `commit_success` never fires (work left uncommitted in the tree), and the steer itself is swallowed as a buffered system message by `compact_context`.

**Fix (small, preserves "never stall"):** in `compact_then_dispatch_next`, before `run_all_dispatch_next`, re-check `state.backlog.run_all` — if `None` or `stop` is set, log and skip the dispatch (end the run if needed). Add a regression test pinning "stop during the wait ⇒ no dispatch".

### LOW-3: Stale `current_item` across the compaction window — an interactive chat turn resolves against the already-completed item

During the window, run-all is `Some` with `current_item` still pointing at the just-resolved (Done/Failed) item, and the main agent is idle with the chat input enabled. A user chat prompt runs as a normal turn; its resolution enters the run-all branch, finds the already-Done item, and — chat turns carry no workflow transition — hits the gate-fail arm (run_all.rs:1509–1518): the **Done item is annotated "plan loop did not close…"** and the run ends prematurely. If the chat turn happens to close a plan loop, the gate passes and `commit_success` + the Done transition run **again** on the already-Done item (done-counter overshoot). Pre-change this window was synchronous (unreachable); the change makes it minutes long.

**Fix:** clear `current_item` (set `None`) once the item is resolved, before spawning the compact task — the `None` state is already handled gracefully by the resolution path (the dispatch-deferral path uses exactly this, run_all.rs:1275–1280). Note: with `current_item` cleared, an interactive turn's resolution re-enters the auto-compact gate and would spawn a second compact task; the busy guard in `run_all_dispatch_next` self-corrects the double dispatch, but consider whether that residual is acceptable.

### LOW-4: README mention

README.md:123 documents the compaction knobs (`[context]`: `summarize_at_fill_rate`, `preflight_compact`, … "also settable in Settings → Advanced") but the new `auto_compact_on_plan_complete` (`[general]`, Settings → Advanced) is not mentioned anywhere. The mirrored precedent (`enable_browser_inspection`) is also absent from the README, so this is a judgment call — but since the README's config paragraph already covers compaction behavior and the run-all loop, a one-phrase mention of `auto_compact_on_plan_complete` (default false; compacts the main agent between run-all items) would fit naturally there.

---

**Notes for the fixer:** HIGH-1 is a one-line fix plus a `tsc --noEmit` run. HIGH-2 and LOW-3 share the theme "the between-items window is now long" — HIGH-2's run-state re-check and LOW-3's `current_item` clear are complementary; both are small. After fixes, re-run `cargo test` (workspace) and `npx tsc --noEmit && npx vitest run` in frontend/.
