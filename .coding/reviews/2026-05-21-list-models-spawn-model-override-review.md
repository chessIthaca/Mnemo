# Review: `list_models` tool + `spawn_agent` model override

**Date:** 2026-05-21
**Scope:** All uncommitted changes (`git diff HEAD`).
**Feature:** A `list_models` discovery tool + an optional `model` parameter on
`spawn_agent` that forces the spawned agent onto a specific model.

## Files reviewed

Feature code:
- `src/model_resolver.rs` — `ModelInfo`, `list_models()`, `resolve_model_id()` + 4 tests
- `src/tool/agent/list_models.rs` (NEW) — the read-only AutoRun tool + 3 tests
- `src/tool/agent/mod.rs` — module registration
- `src/tool/agent/spawn_agent.rs` — `model` arg, `model_resolver` field, `with_model_resolver`, 4 model-override tests + mocks
- `src/agent/factory.rs` — conditional `ListModelsTool` registration, `with_model_resolver` wiring, 1 test
- `src/agent/loop_impl.rs` — `forced_model` field + `set_forced_model`/`forced_model` + short-circuit in `resolve_turn_provider`
- `src/runtime/mod.rs` — `ParentAwareSpawner::spawn_with_parent` gains `model` param
- `src-tauri/src/ipc/spawn.rs` — thread `model` through `spawn_agent_shared`/`IpcSpawner`; apply via `set_forced_model`
- `src-tauri/src/main.rs` — main agent spawn passes `None`

Out of scope (pre-existing bookkeeping): `.coding/safety.toml`, `.coding/plans/*`.

## Verdict

The diff is **substantially correct and constitution-compliant**. The core
mechanism (forced-model short-circuit, trait blast radius, read-only tool,
error-on-unknown, None on existing paths, lock ordering) is sound. Three
findings below — one medium (test gap on the most behavior-critical path), two
low (latent fragility + a doc/behavior mismatch). None block the commit.

## Findings

### Medium — correctness / test coverage

**No test exercises the `forced_model` short-circuit in `resolve_turn_provider`.**
`src/agent/loop_impl.rs:343-365`

The feature's entire user-visible effect — "a spawned agent runs on the
requested model for every turn" — depends on `resolve_turn_provider` checking
`forced_model()` *first* (line 351) and building via
`resolver.build_turn_provider(&forced, ...)` (line 352), bypassing the
subagent/state/skill chain. This is correct by inspection, but nothing pins it:

- The `spawn_agent.rs` tests (`model_arg_resolves_and_is_passed_to_spawner`,
  `unknown_model_id_errors`, etc.) verify the model is **forwarded to the
  spawner** and that `set_forced_model` would be called — they stop at the
  spawner boundary and use a `ModelRecordingSpawner` mock, so they never assert
  the loop *uses* the forced model at turn time.
- The `model_resolver.rs` tests verify `resolve_model_id` resolves correctly.
- `src/agent/tests.rs:2378` (`resolve_turn_provider_uses_override_when_configured`)
  tests the *normal* override path, not the forced path.

A regression that reorders the `forced_model` check below the `resolver.resolve`
call (or drops it) would pass every existing test while silently breaking the
feature. Recommend adding a test that constructs a loop with a resolver
configured for a *state override* (e.g. `[models.planning]`), sets
`set_forced_model(...)` to a *different* model, and asserts
`resolve_turn_provider` returns the **forced** provider, not the state override.
This is the one finding I'd ask to address before commit.

### Low — correctness (latent)

**`resolve_turn_provider` silently drops a forced model when no `model_resolver`
is wired.** `src/agent/loop_impl.rs:348-353`

```rust
let resolver = self.model_resolver.as_ref()?;   // line 348 — returns None if no resolver
if let Some(forced) = self.forced_model() {      // line 351 — never reached
    return resolver.build_turn_provider(&forced, self.fill_rate);
}
```

If `model_resolver` is `None`, the `?` at line 348 returns `None` *before* the
forced-model check, so a forced model would be silently ignored (turn uses the
default provider) rather than honored or errored.

**Reachability:** not reachable via the current spawn path — `set_forced_model`
is only called from `src-tauri/src/ipc/spawn.rs:114`, and the factory wires
`model_resolver` into both the spawn tool and the loop together
(`src/agent/factory.rs:362-368` and `:447-457`), so a loop with a forced model
always has a resolver. But `set_forced_model` is `pub` (loop_impl.rs:296), so a
future caller could set a forced model on a no-resolver loop and get silent
fallback. Consider either moving the `forced_model` check above the
`model_resolver.as_ref()?` (and erroring/returning None explicitly when a forced
model is set but no resolver can build it), or documenting the precondition.
Low because currently unreachable.

### Low — correctness / doc mismatch

**A forced model is not guaranteed for the agent's lifetime if its endpoint is
deleted from config after spawn.** `src/agent/loop_impl.rs:291-296, 334-338` ↔
`src/model_resolver.rs:212-244`

`set_forced_model`'s doc says the forced model "stays in effect for the agent's
lifetime", and `resolve_turn_provider`'s doc says it makes the agent "run on the
requested model for every turn, regardless of workflow state changes". But the
implementation delegates to `resolver.build_turn_provider(&forced, ...)`, which
returns `None` when the referenced endpoint no longer exists in config
(`ConfigModelResolver::build_turn_provider` → `config.resolve_model_ref` drops
a dangling endpoint, `src/config/mod.rs:91-98`). On `None`,
`resolve_turn_provider` returns `None` and the turn **silently falls back to the
default provider**.

This is *consistent* with the existing state-override path (line 364 has the same
None-fallback semantics), so it's not a regression. But it does contradict the
"for every turn / agent's lifetime" wording. Either soften the doc to "for every
turn while the endpoint remains configured", or decide a forced model should
error/hard-fail if its endpoint vanishes (probably overkill). Low.

### Informational (no action needed)

- **`resolve_model_id` trailing validation is redundant but harmless.**
  `src/model_resolver.rs:282-285` — after finding the serving endpoint by
  scanning `config.endpoints`, it rebuilds a `ModelRef` and calls
  `config.resolve_model_ref(Some(&model_ref))` to "validate the endpoint still
  exists". It always exists (just read from the same config), so this always
  returns `Some`. The comment acknowledges this ("it does — we just read its
  name"). It normalizes through config's resolver for consistency, which is fine.
  No action.

- **`resolve_model_id` is case-sensitive and first-match-wins.** Both are
  documented (trait doc `model_resolver.rs:113-114`, impl comment `:267-270`).
  Exact, case-sensitive matching is correct for model ids. Acceptable.

## Concerns explicitly checked — all clean

- **`forced_model` short-circuit bypasses the resolve chain but still uses the
  resolver to BUILD the provider.** ✓ `loop_impl.rs:351-352` — checks
  `forced_model()` first, then `resolver.build_turn_provider(&forced, ...)`.
  Correct.

- **`is_subagent` interaction.** ✓ A forced-model subagent is still marked
  `is_subagent` and still has plan mutations denied: `set_forced_model`
  (`ipc/spawn.rs:113-115`) runs *before* the `if parent_id.is_some()` block
  (`:117-127`) that calls `set_plan_mutations_allowed(false)` and
  `set_is_subagent(true)`. The forced-model check in `resolve_turn_provider`
  returns before reading `is_subagent()` (line 354), so it correctly skips the
  `[models.subagent]` override — but `is_subagent` is set at spawn time
  independently, so plan-mutation policy is unaffected. `is_subagent()` is read
  *only* at `loop_impl.rs:354` (to build `ModelContext`), so no other consumer is
  impacted. Correct.

- **`ParentAwareSpawner` trait signature blast radius — all impls updated.** ✓
  Three impls exist and all carry the new `model: Option<ModelRef>` param:
  `IpcSpawner` (`ipc/spawn.rs:213`), `ParentAwareMock` (`spawn_agent.rs:315`,
  `_model`), `ModelRecordingSpawner` (`spawn_agent.rs:437`). No other impls.
  Plain `AgentSpawner::spawn` (`IpcSpawner::spawn`, `ipc/spawn.rs:194-197`)
  delegates to `spawn_with_parent(name, task, None, None)` — correctly passes
  `None`, ignoring model. ✓

- **`list_models` is read-only / AutoRun.** ✓ `list_models.rs:64-66` returns
  `SafetyLevel::AutoRun`; `execute` only reads config via `resolver.list_models()`
  — no writes, no approval. No side effects.

- **Unresolvable model id errors, doesn't silently fall back.** ✓
  `spawn_agent.rs:158-163` — `resolve_model_id` returning `None` yields
  `ToolResult::error("unknown model id '{model_id}' — call list_models ...")`.
  Default `resolve_model_id` returns `None` (`model_resolver.rs:117`), so a
  no-resolver tool also errors ("model selection unavailable",
  `spawn_agent.rs:155-157`). Confirmed by `unknown_model_id_errors` and
  `model_arg_without_resolver_errors` tests.

- **Main agent + UI spawn paths pass `None`.** ✓ `main.rs:111` (main agent),
  `ipc/spawn.rs:46` (UI `spawn_agent`), `ipc/spawn.rs:197` (plain
  `AgentSpawner::spawn`). No behavior change for existing spawns.

- **Lock ordering / deadlock.** ✓ `set_forced_model` (`loop_impl.rs:297-302`)
  and `forced_model()` (`:308-313`) lock only the `forced_model` mutex.
  `resolve_turn_provider` calls `forced_model()` (lock+release) then, only on
  the non-forced path, `is_subagent()` (separate mutex, lock+release) — the two
  are never held simultaneously. `build_turn_provider` acquires the resolver's
  cache lock after `forced_model` is released. The caller in `turn.rs:76-78`
  holds the workflow lock across the call, but neither `forced_model` nor
  `is_subagent` touches workflow, and no path acquires workflow after these
  mutexes. No deadlock or lock-ordering hazard.

- **Security — no secret leakage.** ✓ `list_models` exposes only endpoint
  *names* and *model ids* (`ModelInfo { endpoint, models }`,
  `model_resolver.rs:35-40`). It does NOT expose `base_url`, `kind`, API keys,
  or pricing. Keys live in `keys.toml` and are never read by this path.

- **Constitution — doc comments.** ✓ All new public items have doc comments:
  `ModelInfo` + fields, `ModelResolver::list_models`/`resolve_model_id`,
  `ListModelsTool::new`, `SpawnAgentTool::with_model_resolver`,
  `AgentLoop::set_forced_model`/`forced_model`,
  `ParentAwareSpawner::spawn_with_parent`.

- **Constitution — line endings.** ✓ No mixed-ending introduction in `.rs`
  source. The git LF→CRLF warnings are on `.coding/plans/*.md` and
  `.coding/safety.toml` (bookkeeping), not source.

- **Constitution — no commit to main.** ✓ N/A to the diff itself (commit is a
  later step on the feature branch).

## Recommendation

Address the **medium** finding (add a `resolve_turn_provider` forced-model
test) before commit. The two **low** findings can be addressed by a one-line doc
softening or deferred — neither is a correctness bug in the current reachable
code paths.
