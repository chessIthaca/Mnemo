# Code Review — Per-Workflow-State Model Overrides: Effective Model + Reviewing Slot

**Date:** 2026-04-16
**Reviewer:** read-only reviewer subagent
**Scope:** all uncommitted changes (`git diff HEAD` + `git status --short`) for the plan
"Fix per-workflow-state model overrides: surface effective model + add Reviewing slot".
Changed files: `src/agent/loop_impl.rs`, `src-tauri/src/ipc/agent.rs`,
`src/config/general.rs`, `src/model_resolver.rs`, `src-tauri/src/ipc/settings.rs`,
`frontend/src/lib/tauri.ts`, `frontend/src/components/settings/sections/ModelsSection.tsx`,
`frontend/src/components/layout/StatusBar.tsx`,
`frontend/src/lib/ipc-fixtures/dto-get-settings.json`, plus plan bookkeeping
(`.coding/plans/*`) and the untracked diagnosis artifact
`.coding/analysis/per-state-model-diagnosis.txt`.

**Plan goal (as understood by this reviewer):** the `[models]` per-state resolution
chain was already correct per turn, but nothing surfaced it — `AgentInfo.model` read
the default provider slot and the StatusBar showed the global default — so the user
never saw overrides in effect. Also `ModelsConfig` had no `reviewing` field, so a
`reviewing` key in `config.toml` was silently dropped by serde and erased on save.

---

## Focus-area verification

### F1 — `resolved_model` is set on EVERY return path of `resolve_turn_provider` ✓

`src/agent/loop_impl.rs:437-486`. All four return paths were traced in the actual
code:

1. **Forced-model + resolver present** (lines 449-456): builds, then
   `set_resolved_model(built.as_ref().map(|(p,_)| p.model().to_string()))`, then
   `return built`. Set runs unconditionally before the return; when `built` is
   `None` (endpoint dangling) the set stores `None` — correct, because the caller
   falls back to the default provider in that case (`turn.rs:78-81`).
2. **Forced-model set but no resolver** (lines 457-460): falls through — no
   `return`. Execution reaches the `let Some(resolver) ... else` at line 461,
   which sets `None` and returns. No stale value survives. ✓
3. **No resolver attached** (lines 461-465): `set_resolved_model(None); return None;`. ✓
4. **Resolver returns `None`** (lines 472-476): `set_resolved_model(None); return None;`. ✓
5. **Resolver returns `Some`** (lines 481-485): sets from `built` before the
   tail-expression return. ✓

The conversion of the previous `self.model_resolver.as_ref()?` early-return into
`let ... else { set; return None; }` removes the one path that previously skipped
the set. No `?` or early `return` remains between the forced-model block and the
final tail expression other than the ones listed. A prior override's value
**cannot** survive into a subsequent default-fallback turn. ✓

Cross-checked the sole caller `src/agent/turn.rs:75-82`: `resolve_turn_provider` is
called exactly once per turn, before the turn runs, so `resolved_model` always
reflects the most recent turn's provider. ✓

### F2 — `AgentInfo.model` fallback semantics ✓

`src-tauri/src/ipc/agent.rs:332-338`:
`l.resolved_model().unwrap_or_else(|| l.provider().model().to_string())`.

- Before the first turn: `resolved_model` is `None` (both constructors init
  `Mutex::new(None)`, loop_impl.rs:221,262) → reports the default provider's
  model. Correct: that is what the first turn *would* run on absent overrides.
- After a turn on the default: `resolved_model` is `None` → same fallback.
  Consistent — the reported model equals the model actually used.
- After a turn on an override: `Some(override)` → reported. ✓
- Loop removed (agent exited): outer `.map()` on `agent_loops.get(&id)` yields
  `None`, `#[serde(skip_serializing_if)]` keeps the field absent — unchanged
  behavior, and the frontend's `registerAgents`
  (`useAgentStore.ts:502-504`) correctly refuses to clobber a known model with an
  absent one. ✓

`AgentInfo.model` doc comment (agent.rs:129-133) updated to match the new
semantics. ✓

Note (pre-existing, not introduced here): the `resolved_model().unwrap_or_else(...)`
expression is evaluated *inside* the `agent_loops` lock (agent.rs:329); the two
mutex acquisitions (`resolved_model`, and inside `provider()` if it locks) are
short and non-nested with the `agent_loops` async mutex in a cycle-forming way
(turn.rs never takes `agent_loops`), so no deadlock risk is added. The lock
ordering is the same as the previous `provider().model()` call.

### F3 — Serde wire shapes stay in sync ✓

- `ModelsConfigWire` (settings.rs:626-640): `reviewing` field added; emitted as
  `null` when unset (struct comment at 558-560 confirms no `skip_serializing_if`
  on these structs — so the key is always present on the wire).
- `models_config_wire()` (settings.rs:778-789): emits `reviewing` from the live
  config. ✓
- `ModelsConfigDto` (settings.rs:924-939): `reviewing` added with the same
  `#[serde(default, deserialize_with = "deserialize_optional_nullable")]` absent/
  null/set triple semantics as the other slots. ✓
- Validation loop (settings.rs:1138-1148): `&models.reviewing` included in the
  fixed-slot array — an invalid reviewing endpoint is rejected like the others. ✓
- Patch application (settings.rs:1206-1208): `if let Some(r) = models.reviewing {
  general.models.reviewing = r.map(to_ref); }` — mirrors the other slots. ✓
- Wire render tests (settings.rs:1546, 1581, 1615) assert `models.reviewing` is
  emitted as null; DTO patch-semantics test (settings.rs:1710-1721) exercises
  set/clear for reviewing. ✓
- Golden fixture `frontend/src/lib/ipc-fixtures/dto-get-settings.json` gains
  `"reviewing": null` inside `models`. ✓ (Key order: serde emits struct-field
  order — `planning, executing, reviewing, complete, subagent, skill` — while the
  fixture is alphabetical; JSON object key order is not significant for equality
  in the frontend contract test, which does property assertions, not a byte
  compare — see ipc-contract.test.ts:335-355.)
- TS types (`tauri.ts:248-262` and the `SettingsSavePatch` at 306-313) mirror the
  wire shape exactly. ✓

### F4 — Reviewing back-compat ✓

`src/model_resolver.rs:213`:
`WorkflowState::Reviewing => models.reviewing.as_ref().or(models.executing.as_ref())`
— unset `reviewing` falls back to the `executing` override, preserving pre-slot
behavior exactly (previously the arm read `models.executing` directly). New test
`reviewing_state_resolves_with_fallback_to_executing` (model_resolver.rs:467-501)
covers all three cases: reviewing set, reviewing unset + executing set, both unset
→ `None`. ✓

Config round-trip: `ModelsConfig` derives `Serialize/Deserialize` with
`#[serde(default)]` at the struct level (general.rs:107-108), so the new field
parses from and persists to `config.toml`; `models_section_round_trips`
(general.rs:469-502) round-trips a set `reviewing` through TOML;
`models_section_save_round_trips_through_file` (general.rs:505-530) exercises the
file save/load path with `reviewing: None`. The previously-reported silent-drop of
a `reviewing = {...}` key is fixed. ✓

### F5 — Frontend ✓

- `StatusBar.tsx:36-37`: `effectiveModel = activeAgent !== null ? (agentModels[activeAgent] ?? model) : model` — falls back to the global default `model` when there is no active agent or no known model for it. The label (lines 412-419) renders `effectiveModel` with an explanatory `title` noting the picker selects the default; the picker highlight logic is unchanged (still tracks the store's `model`). `agentModels` is populated from `AgentInfo.model` in `registerAgents` (useAgentStore.ts:502-504), which now carries the effective model — so the StatusBar shows the override once the agent has run a turn and `list_agents` has been polled. This matches the stated intent; freshness depends on the existing `list_agents` poll cadence (pre-existing behavior for the tab second line, MainPanel.tsx:63 — same data source). ✓
- `ModelsSection.tsx`: `FIXED_SLOTS` gains the Reviewing row; `Draft`,
  `emptyDraft`, `draftFromSettings`, and the save payload all gain `reviewing` —
  types are consistent with the `ModelsConfig`/`SettingsSavePatch` TS changes.
  Slot ordering in the UI (Planning, Executing, Reviewing, Complete, Subagents)
  matches the workflow lifecycle. ✓
- Fixture change (`dto-get-settings.json`) is the only JSON drift. ✓

### F6 — Constitution compliance ✓

- **No `#[allow(...)]` suppressions added.** The full diff contains no `#[allow`.
  The only `#[allow(...)]` in the tree (`src/agent/loop_impl.rs:191,230` —
  `clippy::too_many_arguments` on the two `AgentLoop` constructors) is
  pre-existing and untouched.
- **Doc comments on public items:** new public fns `set_resolved_model` /
  `resolved_model` (loop_impl.rs:362-382) have doc comments; the new
  `pub(crate)` field has a doc comment; the new `reviewing` fields on
  `ModelsConfig`, `ModelsConfigWire`, `ModelsConfigDto`, and the TS interfaces
  all carry doc comments. ✓
- **Warning-free build:** I cannot execute `cargo test`/`tsc` myself (read-only,
  no shell); the reported state is 755 lib + 15 integration tests green, `tsc` +
  `vite` + 131 vitest green. The main agent must confirm `cargo test` passes
  unpiped before closing (per the standard closing sequence). No compile-level
  hazard was found in the diff (see L1/L2 below for the two items I checked
  closely that could have been warnings).
- **Line endings:** the git warning about LF→CRLF applies only to the pre-existing
  plan file `.coding/plans/2ca99f43-*.md` (auto-checksummed bookkeeping, one-line
  checkbox flip), not to any source file in this diff.

---

## Findings

### Correctness — none blocking

**C1 (low) — Stale effective model briefly visible across a config save.**
`resolved_model` records what the *last turn* used; `AgentInfo.model` reports it
verbatim. When the user edits `[models]` in Settings (e.g. changes the Executing
override from model A to model B), `sync_model_resolver` (settings.rs:540) updates
the live config, but `resolved_model` still says "A" until the agent's next turn.
The StatusBar/tab therefore show the previously-used model between the save and
the next turn — arguably *correct* ("what actually ran most recently") and the
doc comments say exactly that, but a user testing their new override by reading
the StatusBar immediately after saving may think the save didn't take. Given the
label's `title` says "resolved for its most recent turn", this is documented
behavior; noting it only as a UX observation, no fix required.

**C2 (low) — StatusBar `effectiveModel` for the *active* agent only.**
By design (`StatusBar.tsx:37`), switching agent tabs changes which model is
shown. When no agent is active it shows the global default. This matches the
plan goal; just confirming the `activeAgent !== null` guard means the default
model (not an arbitrary agent's) is shown with no selection. ✓ No issue.

### Bugs — none found

Specifically checked:

- **No early `?`/return skips the set** in `resolve_turn_provider` (the one
  `?` in the old code, `self.model_resolver.as_ref()?`, was converted to a
  `let ... else` that sets `None`). See F1.
- **Mutex poisoning:** `set_resolved_model`/`resolved_model` use
  `.expect("resolved_model lock poisoned")` — consistent with every other
  accessor in this file (`forced_model`, `last_review_report`, etc.). The lock
  is never held across an await, so poisoning can only result from a panic
  while the guard is held (a pure assignment — effectively impossible), and the
  established codebase style is to propagate via expect. ✓
- **No deadlock:** `resolved_model` (std Mutex) is taken inside the async
  `agent_loops` lock in `list_agents` (agent.rs:329-341) and, on the write side,
  inside a turn with the workflow async mutex held (`turn.rs:76-78` →
  `resolve_turn_provider` → `set_resolved_model`). The std Mutex guard is
  dropped immediately in both accessors (no guard is held across `.await` —
  `resolved_model()` clones and returns by value). No path takes these locks in
  the reverse order. ✓
- **Forced-model + build failure:** when `build_turn_provider` returns `None`
  (endpoint deleted), the forced branch sets `resolved_model(None)` — correct,
  since the caller falls back to the default provider; the UI then shows the
  default model, which is what the turn actually ran on. ✓

### Security — none

No new input surface: the `reviewing` DTO field goes through the same endpoint-
existence validation as all other model slots (settings.rs:1138-1148). No
secrets in the wire struct (it serializes endpoint *names* + model ids only,
same as before). No shell-out, no path handling, no new IPC commands.

### Constitution compliance

1. **L1 (informational) — Intra-doc link `[`AgentInfo`](crate::AgentInfo)` in
   `src/agent/loop_impl.rs:376` does not resolve within the `myharness` crate.**
   `AgentInfo` is defined in the *separate* `src-tauri` binary crate
   (`src-tauri/src/ipc/agent.rs:126`); the lib crate has no `crate::AgentInfo`
   (verified: `search` for `pub struct AgentInfo` and `AgentInfo` in `src/lib.rs`
   — no matches). The field doc comment at loop_impl.rs:125 correctly uses
   backticks without a link; only the `resolved_model()` method doc has the
   `crate::AgentInfo` link target. Impact: `rustdoc` would flag this as a broken
   intra-doc link *if* `cargo doc` is run — but `broken_intra_doc_links` is a
   rustdoc lint, not a rustc warning, so it does **not** affect `cargo build` /
   `cargo test` even under `#![deny(warnings)]` (verified `#![deny(warnings)]` is
   the only inner attribute at `src/lib.rs:1`, and no `deny(rustdoc::...)` is
   set anywhere in `src/`). If the project ever runs `cargo doc` in CI with
   `-D warnings`, this link would fail. Recommended one-word fix (not blocking):
   change `[`AgentInfo`](crate::AgentInfo)` to plain `` `AgentInfo` `` to match
   the field doc's style.

2. **Warning-free build** — no new warnings visible from the diff: all new
   fields are initialized in both constructors; all new accessors are used
   (`resolved_model()` is read in agent.rs:336; `set_resolved_model` is called
   at loop_impl.rs:454,463,474,484); the new `reviewing` fields are read in
   model_resolver.rs:213, settings.rs (emit/validate/apply/tests), and the
   frontend. No unused imports introduced (the diff adds none). The main agent
   must still run `cargo test` unpiped to confirm. ✓ (with the L1 caveat above,
   which is rustdoc-only).

3. **Doc comments on public functions** — present for all new public items.
   ✓

4. **Line-ending style** — source files unchanged in ending style; the only
   CRLF warning from git is on the pre-existing plan bookkeeping file. ✓

---

## Summary

The change correctly fixes both halves of the reported bug: (1) the effective
per-turn model is now recorded on every resolution path and surfaced through
`AgentInfo.model` → `agentModels` → StatusBar/agent tabs; (2) the `Reviewing`
slot exists end-to-end (config → resolver with executing fallback → wire emit →
DTO patch → validation → TS types → settings UI → fixture). Back-compat holds on
every layer (unset reviewing == old executing-mapping behavior; `#[serde(default)]`
keeps old configs loading; the wire always emits the key).

**Findings requiring a fix: one, low severity** — L1: replace the non-resolving
intra-doc link `[`AgentInfo`](crate::AgentInfo)` at `src/agent/loop_impl.rs:376`
with a plain-code span (the referenced type lives in the other crate; rustdoc-only
concern, does not affect `cargo test`). C1/C2 are observations, not defects.
Everything else: **no findings**.