# Review: plan c0c3ba50 — agent backlog_add emits backlog://changed (branch fix/backlog-add-live-event)

Reviewed all uncommitted changes via `git diff HEAD` + `git status --short`:
`src/tool/workflow/backlog.rs`, `src/agent/factory.rs`, `src-tauri/src/main.rs`,
`src/tool/mod.rs`, `.coding/backlog.json`, `.coding/plans/stack.json`, plus the
untracked plan file `.coding/plans/c0c3ba50-*.md`.

## Findings

### 1. LOW — `set_backlog` doc comment not updated to mention the notifier (src/agent/factory.rs:298-307)

The plan's step 2(5) explicitly required updating the `set_backlog` doc comment
("lines 286-295 … mention the notifier + that it is injected by the IPC layer so
agent adds emit backlog://changed"). The delivered doc still describes the
wiring as store-only: "Wire in the shared backlog store that backs the
`backlog_add` tool" — it never mentions that the wiring slot also carries the
optional `on_changed` notifier (set via `set_backlog_notifier`). The field doc
(factory.rs:127-135) and `set_backlog_notifier`'s own doc (:321-325) do cover
the seam, so this is a completeness gap, not a correctness issue — but per the
project constitution ("a feature that ships with its docs not updated is an
incomplete change") the setter doc should mention the notifier, e.g. a sentence
like "The wiring slot also carries an optional UI notifier injected later via
`set_backlog_notifier` so agent adds emit `backlog://changed` live."

**Fix:** add one sentence to the `set_backlog` doc comment (factory.rs:298-307)
referencing `set_backlog_notifier` and the notifier seam.

## Verified correct (no findings)

### Correctness
- **Exactly-once per successful add, never on failed adds**: `execute()`
  (backlog.rs:121-159) returns early on empty/whitespace text (:129-131) and
  oversize text (:132-137) before the notifier; the notifier fires once at
  :144-146 after the successful `add`. Pinned by
  `notifier_fires_after_each_successful_add` (backlog.rs:242-272), which
  asserts 1 → 2 → still 2 across two adds + one rejected add.
- **No deadlock**: the `MutexGuard` from `self.store.lock().await` at :139 is a
  statement temporary, dropped before the notifier runs at :144. The notifier's
  spawned task re-locks the same store Arc via `emit_backlog_changed`
  (backlog_cmds.rs:68) — safe. Comment at :140-143 documents this.
- **IpcState managed before any agent turn**: the notifier closure (main.rs:342-349)
  only captures `app.handle().clone()`; `app.state::<IpcState>()` runs inside
  the spawned task at tool-execution time. The main agent is spawned with
  `initial_prompt: None` (main.rs:365) and waits on `cmd_rx` (spawn.rs:172);
  `app.manage(IpcState {…})` runs at main.rs:396 before any prompt can reach an
  agent, so the state is always managed when the notifier fires. Comment at
  :337-340 documents this assumption.
- **set_backlog_notifier no-op without a store**: factory.rs:326-331 guards on
  `slot.as_mut()`; main.rs calls `set_backlog` (:329) before
  `set_backlog_notifier` (:350). No panic path.
- **No double/missed emission**: agent path (tool → notifier) and UI path
  (IPC `backlog_add` command → `emit_backlog_changed` at backlog_cmds.rs:97) are
  disjoint; the notifier fires exactly once per agent add. The event channel
  `backlog://changed` matches the frontend listener
  (frontend/src/lib/tauri.ts:28, useAgentEvents.ts:121).
- **All construction sites updated**: factory.rs:683, tool/mod.rs:648,
  backlog.rs:169 (helper), backlog.rs:253 (new test) — all use
  `BacklogAddTool::new`; no stale tuple-struct `.0` accesses remain (all
  updated to `.store` at backlog.rs:220, 233, 239, 282).
- **Notifier is `Fn` (not `FnMut`)**: the closure only clones the AppHandle and
  spawns — correct bound; `tauri::async_runtime::spawn` requires `Send +
  'static`, satisfied (AppHandle is Send + 'static).

### Security
- The notifier closure captures only the `AppHandle` (main.rs:342-349) — no
  secrets, no state handles. No new `unsafe` code anywhere in the diff.

### Docs sync
- backlog.rs module docs (:14-22): the stale "no Tauri event emitter" claim is
  replaced with the notifier description. ✓
- Tool schema description (:92-103): "appears in the Backlog tab immediately
  (the app layer emits a backlog://changed event)" — the refresh/restart claim
  is gone. ✓
- factory.rs field doc (:127-135) + register-site comment (:670-676): updated. ✓
- README.md:48 ("the agent can capture items itself via `backlog_add`") remains
  accurate; PLAN.md needs no change (no provider-strategy/technical-decision
  impact). ✓
- Exception: the `set_backlog` doc gap in Finding 1.

### Multi-platform neutrality
- No Windows-only APIs, paths, or shell syntax. Pure Rust + Tauri cross-platform
  code (`Arc<dyn Fn() + Send + Sync>`, `tauri::async_runtime::spawn`). Builds on
  macOS + Windows. ✓

### Constitution compliance
- No `#[allow(...)]` added; `deny(warnings)` unaffected. ✓
- Regression tests for the defect: `notifier_fires_after_each_successful_add`
  and `no_notifier_is_a_noop` (backlog.rs:242-283) pin the new behavior — the
  old tuple struct had no notifier at all, so these tests fail without the fix.
  The delivered tests are strictly stronger than the plan's step 1(6) ask
  (exactly-once + no-fire-on-reject + None-noop). ✓
- Doc comments on all new public items: `BacklogAddTool` struct (:55-66),
  `BacklogAddTool::new` (:69-70), `set_backlog_notifier` (:321-325). ✓

## Observations (non-findings)

- `.coding/backlog.json` gains item 69 (the PandaFilter idea) and bumps
  `next_id` to 70. This is user-owned backlog state, consistent with the
  tool's purpose; plausibly a live-test artifact of the fix. Not a code
  concern.
- `.coding/plans/stack.json` + the untracked plan file are bookkeeping. ✓
