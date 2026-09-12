# Review — Per-context model selection foundation

**Date:** 2026-05-20
**Branch:** `feat/safety-ux-core-op`
**Scope:** All uncommitted changes (`git diff HEAD`) — 20 files, +745/-41.

## Summary

Adds a `[models]` config section + `ModelResolver` so each turn can run on a
configured endpoint+model based on workflow state / active skill / subagent
status, falling back to the main agent's default. Resolution priority
(skill > subagent > state > default) is implemented in `ConfigModelResolver`.
The turn driver snapshots the default provider, then asks the resolver for a
per-turn override and builds a throwaway provider+context-manager when one
matches.

## Verification performed

- `cargo test --lib model_resolver` → 7 passed.
- `cargo test --lib client_factory` → 7 passed (incl. `build_provider_for`,
  `context_manager_for_model`).
- `cargo test --lib models_section` → 4 passed (config round-trips).
- `cargo test --manifest-path src-tauri/Cargo.toml` → 56 passed (incl. the
  updated golden `dto-get-settings.json` fixture + settings DTO tests).
- `npx vitest run src/lib/ipc-contract.test.ts` → 29 passed.
- `cargo test --lib --no-run` compiles; only 2 pre-existing warnings in
  `src/runtime/agent.rs` (NOT in this diff — out of scope).
- Confirmed on branch `feat/safety-ux-core-op` (not main).

---

## Findings

### Correctness

**C1 — `WorkflowState::Skill` ignores the underlying state override (by design,
but worth confirming).** `src/model_resolver.rs:144-152`: the state-override
match arm maps `WorkflowState::Skill => None`. So while a skill is active with
no skill-specific override, the `executing`/`planning`/`complete` overrides are
*not* applied — the agent drops to the default model. This is documented in the
arm comment and the module doc, and the test `skill_state_with_no_skill_name_falls_through`
covers the no-override path. If the intent is "a skill active during Executing
should still get the Executing model when no skill override is set," this is a
behavior gap. If the intent is "Skill state has no state field, use default,"
this is correct. **Action: confirm intent.** Not a bug either way, but a
design decision that should be explicit.

**C2 — No deadlock between the workflow async-Mutex and the resolver sync-RwLock.**
`src/agent/turn.rs:75-82` holds `self.workflow` (tokio Mutex) while calling
`resolve_turn_provider`, which takes the resolver's `RwLock<Config>` *read*
lock (sync). I traced every other config-lock acquisition site:
`sync_model_resolver` (settings.rs:1144, 451) takes the resolver *write* lock
but never the workflow lock; `save_settings`/`save_endpoints` take
`state.project.config` (a *separate* `tokio::Mutex<Config>`) but not the
workflow lock. No path acquires Config-then-workflow, so there is no
lock-ordering cycle. **No issue** — recorded because the review focus asked.

**C3 — `is_subagent` is set before the first turn.** `src-tauri/src/ipc/spawn.rs:116`
calls `set_is_subagent(true)` inside the `parent_id.is_some()` block, which runs
*before* the task is spawned (line 124) and before the initial prompt is sent
(line 138). The main agent is spawned with `parent_id: None` (main.rs:108) so it
stays `false`. **Correct.**

### Bugs

**B1 (low) — `ConfigModelResolver::fill_rate` field + `fill_rate()` getter are
dead code.** `src/model_resolver.rs:91,103-105`: the stored `fill_rate` is never
read. `build_turn_provider` (line 159) takes `fill_rate` as a *parameter* (passed
by `AgentLoop::resolve_turn_provider` from `self.fill_rate`, loop_impl.rs:309)
and forwards it to `context_manager_for_model` — it never touches
`self.fill_rate`. The resolver's own copy is set in `new()` (main.rs:405) and
then ignored. Not a correctness bug (the right fill_rate reaches the context
manager via the parameter), but it's misleading: a reader assumes the resolver's
`fill_rate` is authoritative. **Action:** either remove the field + getter from
`ConfigModelResolver` (and the `fill_rate` arg to `ConfigModelResolver::new`),
or have `build_turn_provider` ignore its parameter and use `self.fill_rate`.
Pick one source of truth. (No compiler warning because the items are `pub`.)

**B2 (low) — `AgentLoop::with_is_subagent` builder is dead code.**
`src/agent/loop_impl.rs:243`: defined, never called. The IPC layer uses
`set_is_subagent` (the `&self` setter) instead. **Action:** remove
`with_is_subagent`, or use it in the factory build path for subagents.

**B3 (low) — `AgentLoopFactory::model_resolver_handle()` is dead code.**
`src/agent/factory.rs:183`: defined, never called. The IPC layer reaches the
resolver via `state.runtime.model_resolver` (the concrete `ConfigModelResolver`
stored on `AgentRuntimeContext`), not via the factory's `Arc<dyn ModelResolver>`
handle. **Action:** remove it, or have the IPC layer use it instead of storing
a second handle on `AgentRuntimeContext`.

**B4 (very low) — `Option<Option<ModelRefDto>>` patch semantics are unused by
the UI.** `src-tauri/src/ipc/settings.rs:853-861` + the apply block at
:1062-1073 correctly implement outer-None=keep / inner-None=clear / Some=set.
The frontend `ModelsSection.tsx:156-164` always sends all four fixed slots
(full replace), so the "keep" (outer None) path is never exercised from the UI.
The backend handles it correctly regardless, and a future partial-save caller
would work. **No action required** — noted for test coverage (see T2).

### Security

**S1 — No secret leakage.** `ModelRefWire` / `ModelRefDto` / `ModelsConfigWire`
carry only `endpoint` (name) + `model` (id) — never api keys. `get_settings`
serializes `ModelsConfigWire` with no secrets (verified: the no-secrets guarantee
at settings.rs:761 holds). Api-key resolution for a per-turn provider goes
through the shared `resolve_api_key` (client_factory.rs:23) — stored key →
`OPENAI_API_KEY` env → `ANTHROPIC_AUTH_TOKEN` → `"dummy"` — identical to the
main provider path, so a per-context model inherits the same credential chain.
**No issue.**

**S2 — Dangling endpoint references are safely dropped.** `Config::resolve_model_ref`
(config/mod.rs:83) returns `None` when the endpoint doesn't exist, and
`build_provider_for` (client_factory.rs:96) returns `None` on a missing
endpoint — both cause a fall-back to the default provider rather than building
a client against a missing endpoint. `save_settings` validates referenced
endpoints exist (settings.rs:997-1023). **No issue.**

### Constitution compliance

**CC1 — Doc comments present on all new public functions/types.** Verified:
`ModelRef`, `ModelsConfig`, `Config::resolve_model_ref`, `ModelResolver` trait +
both methods, `ConfigModelResolver::{new,fill_rate,set_config}`,
`build_provider_for`, `context_manager_for_model`, `AgentLoop::{with_model_resolver,
with_is_subagent,set_is_subagent,is_subagent,with_fill_rate,resolve_turn_provider}`,
`AgentLoopFactory::{with_model_resolver,model_resolver_handle}`, wire/DTO structs.
DTO *fields* on `ModelRefDto`/`ModelsConfigDto` lack per-field docs, but this
mirrors the existing `VisionModelDto` style (struct-level doc, bare fields) —
consistent with "follow existing code style." **Compliant.**

**CC2 — No commit to main.** On `feat/safety-ux-core-op`. **Compliant.**

**CC3 — Tests run before marking complete.** This review is part of that
sequence; all relevant test suites pass (see Verification). **Compliant.**

**CC4 — Windows paths / PowerShell syntax.** All shell invocations used
PowerShell. **Compliant.**

**CC5 — Line-ending preservation.** `git status` CRLF warnings are only on
`.coding/*.json` and `.coding/plans/*.md` (git's autocrlf normalization notices
on pre-existing LF files), not on edited source files. New files
(`model_resolver.rs`, `ModelsSection.tsx`) are untracked. No mixed endings
introduced by the edits. **Compliant.**

### Performance

**P1 (medium) — Per-turn provider is built twice when an override resolves.**
`ConfigModelResolver::build_turn_provider` (model_resolver.rs:165-170) calls
`build_provider_for` (builds provider #1, a `reqwest::Client`) and then
`context_manager_for_model`, which *internally calls `build_provider_for` again*
(client_factory.rs:119) just to read `capabilities().max_context`, then discards
the second provider. So every overridden turn constructs two `reqwest::Client`s
and throws one away. `max_context` is available without building a client — it's
`endpoint.kind.capabilities_with_overrides(endpoint.max_context, ...).max_context`
(see provider/mod.rs:99). **Action:** in `build_turn_provider`, build the
provider once and derive the context manager from `provider.capabilities().max_context`
directly (drop the `context_manager_for_model` call), or refactor
`context_manager_for_model` to accept a pre-built `&LlmClient`. This also avoids
the per-turn `reqwest::Client` churn noted in P2.

**P2 (low) — Throwaway provider rebuilt every turn.** Each overridden turn
constructs a fresh `reqwest::Client` (connection pool not reused across turns).
The doc comments acknowledge "throwaway provider," so this is a known trade-off,
but for a long-running subagent doing dozens of turns on an override, the
connection pool is never reused. Acceptable for a foundation; consider caching
the resolved provider keyed by `ModelRef` if this shows up in profiles.

### Test gaps

**T1 (medium) — No test for `AgentLoop::resolve_turn_provider` or the `turn.rs`
wiring.** The unit tests cover `ConfigModelResolver::resolve` (priority order,
dangling refs) and `build_provider_for`/`context_manager_for_model` in
isolation, but nothing verifies that `run_turn` actually *uses* the resolved
provider when one is returned, or falls back to the default snapshot when the
resolver returns `None`. `resolve_turn_provider` is `pub(crate)` and could be
tested with a canned `ModelResolver` impl (the trait exists partly for this —
"tests inject a canned resolver," per model_resolver.rs:61). **Action:** add a
test that injects a `ModelResolver` returning a known `ModelRef` and asserts
the turn's LLM call goes to that model (e.g. via a `CapturingProvider`-style
mock), plus a test that a `None`-returning resolver uses the default provider.

**T2 (low) — No test for the `Option<Option<ModelRefDto>>` "keep" semantics.**
The apply block (settings.rs:1062-1073) handles outer-None=keep, but no test
sends a patch with some fields omitted (outer None) alongside others set/cleared.
The existing DTO tests only construct full `ModelsConfigWire` responses. **Action:**
add a `save_settings` patch test that omits `planning` (keep), sets `executing`,
and clears `subagent` (inner None), then asserts `planning` is unchanged.

---

## Verdict

The change set is **correct and secure**. The per-turn resolution works, the
priority order is right, `is_subagent` is set before the first turn, and there
are no deadlocks or secret leaks. The findings are all low-severity
maintainability/perf/test-gap items:

- **Must-fix before merge:** none (no correctness/security bugs).
- **Should-fix:** P1 (double provider build — easy win), B1 (dead `fill_rate`
  on the resolver — pick one source of truth), T1 (turn-wiring test gap).
- **Nice-to-have:** B2/B3 (remove dead builders/handle), P2 (provider caching),
  T2 (patch-semantics test), C1 (confirm Skill-state intent).

All applicable test suites pass.
