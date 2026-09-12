# Review — Phase 5a: Split ipc/commands.rs + unify spawn (2026-08-11)

**Reviewer:** main agent (self-review). A spawned reviewer subagent failed
before writing its report (the file_write gating bug is fixed, but the
subagent itself errored), so the review was performed directly with the same
tooling.

**Scope:** all uncommitted changes — the Phase 5a Maint H1/M3/M4 refactor
(god-module split, spawn unification, IpcState nested contexts), plus the
in-progress plan files / frontend zustand import change from earlier work.

## Summary

No findings. The split is complete, faithful, and verified: 41 commands
survived, no dangling references, no logic changes, nested-state refactor is
consistent, spawn paths unified, line endings preserved, all tests green.

## Verification performed

### H1 — God-module split (completeness + fidelity)

- Command count: 18 (agent) + 6 (settings) + 6 (files) + 10 (backlog_cmds) +
  1 (spawn) = **41**, matching the original `commands.rs` surface exactly.
- No `ipc::commands` / `commands::` references remain anywhere in
  `src-tauri/src` (search confirmed).
- Moved functions compared against `git show HEAD:...commands.rs` — byte-
  identical bodies. The original file used em-dashes (U+2014) which are
  preserved in the new files (char-code verified, e.g. agent.rs:36).
- Tests moved with their code: `endpoint_dto_tests` + `settings_dto_tests`
  now in `settings.rs`, `extract_tests` in `run_all.rs` — all still pass.
- `mod.rs` rewritten as clean ASCII (it previously contained a stray 0x1A
  control byte artifact; replaced with `->` in the header docs).

### M3 — Spawn unification

- `main.rs` main-agent startup now calls `ipc::spawn::spawn_agent_shared(...)`
  (id allocation first, loop build, channel + task spawn, manager register,
  loop-map insert, no initial prompt) — identical to the previous inline
  block, verified line-by-line.
- `IpcSpawner::new` is wired via `set_spawner` BEFORE the main-agent build
  (line 87), matching the original ordering so the main agent's own
  `spawn_agent` tool works from turn one.
- All three spawn paths (main startup, UI `spawn_agent` command, `IpcSpawner`
  tool) now share `spawn_agent_shared` (pub(crate)).

### M4 — Nested IpcState contexts

- `state.rs` restructured: `IpcState { runtime: AgentRuntimeContext,
  project: ProjectContext, approvals, backlog: BacklogContext }`.
- Regex rewrite (single + multi-line forms) applied to all six command
  modules; a follow-up word-boundary check confirmed **zero** flat
  `state.<field>` accesses remain outside `state.rs`.
- `self.factory` / `self.manager` / `self.agent_loops` in `spawn.rs`
  (IpcSpawner's own fields) were NOT mangled — verified.
- Both `IpcState` construction sites in `main.rs` (success + fallback) use
  the nested contexts.

### Cross-module dependencies

- `events.rs` now calls `crate::ipc::run_all::{on_main_turn_resolved,
  halt_run_all_for_approval}` (updated all 5 call sites).
- `emit_agent_event` + `emit_prompt_dispatched` moved to `events.rs` as
  `pub(crate)`; callers in `agent.rs` (enter_skill) and `backlog_cmds.rs` /
  `run_all.rs` import them correctly.
- `run_all.rs` imports `dispatch_next_impl` + `emit_backlog_changed` from
  `backlog_cmds.rs`; `backlog_cmds.rs` imports `run_all_dispatch_next` from
  `run_all.rs` — no circular module dependency (Rust allows sibling refs).

### Constitution compliance

- Windows paths / PowerShell only.
- Public functions have doc comments (all new modules + moved fns retain
  theirs; `pub(crate)` helpers documented).
- Line endings: all 9 ipc files verified CRLF (0 bare LF) — original style
  preserved.
- Frontend command names unchanged (invoke strings untouched); tsc clean.
- Nothing committed to main; changes staged on the feature branch.

### Tests

- `cargo test` (workspace): 508 + 9 + 5 passed, 0 failed.
- `npm test -- --run` (vitest): 24 passed.
- `npx tsc --noEmit`: exit 0.
- `cargo check`: clean, no warnings.

## Outcome

No findings. Phase 5a is ready to commit.