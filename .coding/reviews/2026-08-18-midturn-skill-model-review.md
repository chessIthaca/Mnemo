# Review: mid-turn skill/state model switching — `fix/skill-model-midturn` @ 7cc284c

**Scope:** all changes in commit 7cc284c (working tree clean, so HEAD == the change):
`src/agent/turn.rs` (per-iteration provider re-resolution at top of the `run_turn` loop),
`src/agent/loop_impl.rs` (doc comment on `resolve_turn_provider`), `src/agent/tests.rs`
(regression test `run_turn_switches_to_skill_model_after_midturn_skill_start`).
`.coding/plans/0437d08e-*.md` bookkeeping reviewed for context.

**Verdict: ship it.** One LOW documentation finding; no correctness, security, or
constitution violations.

---

## Findings

### LOW (docs) — stale "one provider per turn" claim on the `provider` field comment
**`src/agent/loop_impl.rs:68-72`**

```rust
/// The LLM provider. Behind an `RwLock` so the model can be swapped at
/// runtime (e.g. the status-bar model picker) without rebuilding the loop.
/// Read once per turn into a local snapshot so a turn always talks to one
/// provider even if it's swapped mid-flight.
```

Lines 70-71 ("a turn always talks to one provider even if it's swapped mid-flight") are
now **false** in the resolver-override case: this change deliberately lets a turn talk
to a *different* provider per iteration after a mid-turn `skill_start`/`skill_end`/state
transition. The turn.rs comment (turn.rs:56-61, "a single provider request must talk to
one provider throughout") and the `resolve_turn_provider` doc (loop_impl.rs:532-536)
were both correctly updated; this field comment was missed. (Note `set_provider`'s own
doc at loop_impl.rs:732-746 remains accurate — it concerns the default slot, whose
snapshot is still per-turn.)

**Suggested fix** (reword lines 70-71 only):
> Read once per turn into a local snapshot that serves as the turn's *default*
> (fallback) provider — a runtime swap takes effect on the next turn. A per-context
> resolver override may still switch the provider *between* requests within a turn
> (see [`resolve_turn_provider`](Self::resolve_turn_provider)); a single request never
> switches mid-flight.

This is the only change-blocking item and it is a two-line comment edit.

---

## Verified — no defect (points from the review brief)

### 1. Per-iteration re-resolution correctness — `src/agent/turn.rs:149-169`
- **Lock discipline:** the tokio `MutexGuard` (`wf`) is acquired at :157 and dropped at
  the block close (:169). `resolve_turn_provider` is fully synchronous (loop_impl.rs:537-586),
  so **no `.await` executes under the workflow mutex**. The std locks it takes inside
  (`forced_model`, resolver config `RwLock` read, resolver cache `RwLock` read→write on
  miss, `resolved_model`) are all leaf locks held momentarily; no code path holds any of
  them and then awaits the workflow mutex, so no lock-order cycle / deadlock is possible.
  The pattern matches the two other per-iteration workflow-lock sites already in this
  loop (:352-358, :1219-1235).
- **Consistency:** `wf.state()` and `wf.active_skill()` are read under a *single* lock
  acquisition, so the resolver never sees a torn (state, skill) pair across a transition.
- **Uninitialized bindings (:84-85):** the resolution block is the **first statement of
  every iteration**; the loop's only `continue` (:936, malformed-JSON retry) jumps back to
  that statement, and the loop exits exclusively via `return` (no post-loop read). Rust's
  definite-assignment analysis enforces this at compile time — no path can read
  `provider`/`context_manager` before first assignment. Confirmed by inspection as well.
- **Resolver returns `None`:** falls back to `Arc::clone(&default_provider)` + 
  `default_context_manager.clone()`. `ContextManager` is `#[derive(Clone)]` over two
  `usize`s (context.rs:50-54) — cheap and correct. The once-per-turn default snapshot
  preserves the documented "picker swap takes effect next turn" semantics for the
  non-override path.

### 2. Non-skill path — no unintended behavior change
- **`set_resolved_model` per iteration** (loop_impl.rs:554/563/574/584): a momentary
  std-mutex write of an `Option<String>`. The value stays *more* accurate than before —
  it always reflects the provider serving the most recent request (e.g. after a mid-turn
  `skill_end` it drops back to the state override / default on the next iteration).
  Interplay with the IPC `set_model` clear (which writes `None` after a picker swap) is
  benign: if an override is active, the next iteration re-records the override model —
  which is genuinely the model serving requests — so no stale/misleading UI value.
- **`build_turn_provider` per iteration** (model_resolver.rs:220-257): cache-hit path is
  an `RwLock` read + `HashMap` lookup + `Arc::clone` + a two-`usize`
  `ContextManager::new` — negligible per request. Cache miss (full provider build, new
  `reqwest::Client`) happens at most once per `(endpoint, model)` per cache
  invalidation (`set_config`), i.e. no more often than the old once-per-turn code did
  over a turn's lifetime. The Arc identity guarantee (connection-pool reuse) is
  unchanged and still test-pinned (`build_turn_provider_caches_provider_across_turns`).
- **Per-iteration provider coherence:** everything derived from the provider in the loop
  body reads the *same* iteration's binding — `capabilities()` for the volatile tail
  (:356) and tool schemas (:450), `kind()` for `is_local` (:416), the request itself
  (:514), summarization (:221), and the `Usage` model id (:660). No cross-iteration
  mixture. `provider` is assigned only in the resolution block — the "one provider per
  request" invariant holds.

### 3. Mid-iteration summarization — correct, slightly improved
`summarize_at()`/`max_tokens()` are read per iteration from the CM matching the
iteration's provider (:215, :259, :287). If the model switches to a *smaller* window
mid-turn (e.g. into a skill override), summarization triggers on the very next iteration
— with the old turn-start CM this was mis-sized. Switching to a larger window correctly
defers/avoids summarization. The `ContextUsage` event's `max` tracks the current
provider, so the UI bar stays consistent. No regression.

### 4. Regression test quality — `src/agent/tests.rs:3413-3598`
- **Pins the behavior, both directions:** pre-fix, round 2 is served by the default
  snapshot → `outcome.text == "from-default"` → the final assertion fails (empirically
  verified by the author). It also catches *over-eager* switching: if resolution somehow
  applied the skill model before `skill_start` ran, round 1 would return tool-less text,
  `skill_start` would never execute, and the `WorkflowState::Skill` assertion (:3583-3587)
  would fail — the test cannot vacuously pass.
- **No flakiness:** no timers or races — the mock streams are `stream::iter`;
  `MockProvider::sequence` pops deterministically (tests.rs:63-81); `_cmd_tx` and
  `_fanin_rx` are kept alive so no channel-closed branch fires mid-stream; the fan-in
  channel (cap 64) is far larger than the event count and sends ignore errors anyway;
  the resolver's std `Mutex<bool>` is never held across an `await`.
- **Cleanup:** `tempdir()` auto-removes on drop; no plan files are written (no
  `create_plan` call); each test builds its own workflow/registry — no cross-test state.
- **Fidelity:** the skill-scoped resolver mirrors `ConfigModelResolver`'s real
  `[models.skill.<name>]` gate (`ctx.skill_name == Some(skill)`), so the test exercises
  the actual production seam, not a tautology.

### 5. Constitution compliance
- **Doc comments:** no new public items (test + private-fn edits + comment updates).
  Comments updated where semantics changed — except the LOW finding above.
- **No `#[allow(...)]` added:** the `#[allow(clippy::too_many_arguments)]` on the two
  constructors (loop_impl.rs:263, :303) predate this change (parameter lists untouched).
- **Regression test per defect:** present and behavior-pinning. ✓
- **Warning-free build:** author reports `cargo test` green (1009 tests) under
  `#![deny(warnings)]`, which proves zero warnings. The uninitialized-binding pattern
  compiles clean because the compiler verifies definite assignment.

### 6. Security
No new attack surface: the change only moves *when* existing, config-driven resolution
runs (per request instead of per turn). No new inputs, no new privilege, no tool-surface
change. The workflow lock is not held across awaits, so no new stall vector for the UI
or tools.

---

## Informational (no action required)

- **Deliberate semantic worth knowing:** for a skill with **no** `[models.skill.<name>]`
  override, a mid-turn `skill_start` now *sheds* the pre-skill state override on the next
  request (the resolver returns `None` in the Skill state → default model), where pre-fix
  the turn kept the state-override model until the turn ended. This follows the resolver's
  documented design decision (model_resolver.rs:198-207 — the Skill state intentionally
  has no state-override fallback) and the updated turn.rs comments; it is a consequence,
  not a bug. Users with `[models.planning]` set who run a skill without a skill override
  will see the model change mid-turn.
- First iteration after a cache invalidation (`set_config`) builds a provider while the
  workflow tokio mutex is held (one-time, tens of ms, no awaits inside) — at worst a
  one-off brief wait for a concurrent `get_workflow_state` reader. Negligible.
