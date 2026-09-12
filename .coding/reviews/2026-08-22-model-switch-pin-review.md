# Review: Fix model switch not applied on next turn (kimi→deepseek stuck) — 2026-08-22

**Scope reviewed:** all uncommitted changes per `git diff HEAD` + `git status --short`:
`src/agent/loop_impl.rs`, `src/agent/tests.rs`, `src-tauri/src/ipc/config_io.rs`,
`src-tauri/src/ipc/agent.rs`, `README.md`, `.coding/plans/stack.json`, plus the new
(untracked) plan file `.coding/plans/c0bfefc2-1a78-4e07-9bbd-005a854a1045.md`. Supporting
code read for verification: `src/agent/turn.rs:150-270`, `src/agent/loop_impl.rs:340-939`,
`src/model_resolver.rs:1-160`, `src/provider/mod.rs:30-100,341-380`, `src/config/mod.rs:30-149`,
`src/config/general.rs:161-181`, `src/config/endpoints.rs:42-135`, `src-tauri/src/console.rs`
(swap path), `PLAN.md:260-285`, and the existing `swap_provider_into_loop*` tests in
`src-tauri/src/ipc/config_io.rs:515-635`.

**Verdict:** Approve with minor fixes. The fix is correct and well-scoped; the regression test
genuinely exercises the pre-fix failure mode. Two broken intra-doc links, one inaccurate doc
phrase, and one PLAN.md doc-sync omission follow — all low severity, no correctness or
security blockers.

---

## Verified correct (no action needed)

- **Root-cause analysis confirmed against the code.** `run_turn`
  (`src/agent/turn.rs:188-202`) calls `resolve_turn_provider` at the top of every request
  iteration, and pre-change that function consulted the `ConfigModelResolver` chain before
  ever using the default provider slot — so `swap_provider_into_loop`'s plain
  `set_provider` swap was silently overridden by `[models.*]` on the next turn. The
  diagnosis in the plan is accurate.
- **New arm placement is correct and deliberate.** The pinned-provider arm
  (`src/agent/loop_impl.rs:662-670`) runs before the forced-model arm (:678) and before the
  resolver chain (:690-714). A live picker choice beating a spawn-time forced model is the
  right priority, and the doc comment (:624-637) documents the 3-level chain accurately.
- **Pinned-provider reuse is sound.** The pin stores the exact picker-built provider
  (including its resolved reasoning effort), bypassing the resolver's `(endpoint, model)`
  cache key — so picker effort survives, as intended. The same `Arc` is also swapped into
  the default slot, so trace-phase stamping (`record_prep_ms`/`record_compact_ms`,
  `turn.rs:252`) keeps working on the pinned provider.
- **`resolved_model` bookkeeping stays coherent.** The pin arm calls
  `set_resolved_model(Some(model))` (:668); `swap_provider_into_loop` clears it to `None`
  right after pinning (`config_io.rs:232`), so the next iteration's `prev != now` compare
  (`turn.rs:204-213`) emits exactly one `ModelChanged`. Even the `None` fallback
  (`turn.rs:208-209`) reports the right model because `set_explicit_provider` also swaps
  the default slot.
- **Global path confirmed unpinned.** `swap_provider_into_loops` (`config_io.rs:179-200`)
  still uses plain `set_provider` and never touches `explicit_provider` — a global
  factory/console swap cannot undo a per-agent pick. `save_endpoints` routes through
  `swap_live_provider` → `swap_provider_into_loops` (:255), so endpoint edits keep pins.
  This matches the documented intent.
- **Mid-turn behavior matches intent.** The pin arm returns before any
  skill/state resolution on every iteration, so a mid-turn `skill_start` after a picker
  switch keeps the picker model — the code does what the plan says.
- **Lock usage is clean.** Read-lock + clone on `explicit_provider`, guard dropped before
  `set_resolved_model`; `set_explicit_provider` acquires the `provider`/`context_manager`
  write locks (inside `set_provider`) and releases them before taking the
  `explicit_provider` write lock. No lock nesting across the four locks, no new poisoning
  surface beyond the existing uniform `.expect("... lock poisoned")` pattern.
- **Interaction risk assessed — endpoint deletion under a pin: acceptable, not a
  regression.** The forced-model arm falls back when its endpoint is deleted because it
  rebuilds from config every turn; the pinned provider is an already-built client holding
  its own base URL + credentials and never re-reads config, so deletion does not break it.
  The only divergence: if the user deletes an endpoint to *revoke* it, the pinned agent
  keeps calling the old server until the next picker switch — failures surface through the
  normal retry/error path and recovery is one picker action. Deliberate, documented, and
  strictly less confusing than the bug being fixed. Not a finding.
- **Regression test matches the shipped code verbatim.** Every type/signature the test
  builds checks out: `ConfigModelResolver::new(Arc<RwLock<Config>>, Arc<LlmRequestLog>)`
  (`model_resolver.rs:152`), `LlmRequestLog::new()` (`trace.rs:359`), `Config` fields
  (`config/mod.rs:38-49`), `Endpoint` fields incl. `From<&str> for ModelSpec`
  (`endpoints.rs:44-80,117`), `ModelRef`/`ModelsConfig.planning`
  (`general.rs:161,179-181`), the 9-arg `AgentLoop::new` order (`loop_impl.rs:366-376`),
  `make_registry` (`tests.rs:108`), `WorkflowState::Planning`/`Skill` variants, and the
  `LlmClient` trait's four required methods + defaulted `record_*` hooks
  (`provider/mod.rs:341-380`) with the exact 7-field `Capabilities` (:32-49). The
  assertions (unpinned Planning → `"o3"`; pinned Planning and pinned
  `Skill`+`Some("merge_to_main")` → `"deepseek-v4-flash"`; default slot → picked model)
  fail on the pre-fix resolution order and pass now.
- **Both constructors initialize `explicit_provider: None`** (:395, :438).
- **Existing src-tauri tests remain compatible:** `swap_provider_into_loop_swaps_only_the_target_agent`
  asserts the target's `provider()` changed (still true — `set_explicit_provider` swaps the
  default slot too), other loops/factory untouched, `resolved_model` None, unknown-id false.
- **Constitution:** no new `#[allow]` (the `#[allow(clippy::too_many_arguments)]` at
  `loop_impl.rs:365` is pre-existing); the new public method and field carry doc comments;
  pure Rust, no platform-specific code; README bullet (`README.md:48`) is accurate.

---

## LOW

### L1 — Broken intra-doc link in the lib crate: `crate::ipc::config_io` does not exist here

`src/agent/loop_impl.rs:161` (inside the new `explicit_provider` field doc):

```text
/// [`swap_provider_into_loop`](crate::ipc::config_io::swap_provider_into_loop);
```

`src/lib.rs:12-28` has no `ipc` module — the IPC layer lives in the **src-tauri binary
crate**, unreachable from the library. rustdoc's `broken_intra_doc_links` lint is
warn-by-default and is *not* covered by `#![deny(warnings)]` (a rustc lint group), which is
why `cargo test` stays green — but `cargo doc` / doctest extraction emits the warning and
the rendered docs carry a dead link. The constitution requires doc comments; a dead link in
one is a doc defect.

**Fix:** drop the link syntax and name it in prose, e.g.
``Set only by the Tauri IPC layer's `swap_provider_into_loop`;``.

### L2 — Broken intra-doc link in src-tauri: `AgentLoop` is not in scope in `config_io.rs`

`src-tauri/src/ipc/config_io.rs:208`:

```text
/// [`AgentLoop::set_explicit_provider`] so the picker's choice beats the
```

The module's imports (`config_io.rs:15-29`) pull in `ContextManager`, `AgentLoopFactory`,
`Config`, `Endpoint`, `build_client`, `LlmRequestLog`, `LlmClient`, etc. — but never name
`AgentLoop`, so the link's first segment cannot resolve. Same lint class as L1, same
visibility (binary crate, so lower still, but the src-tauri tests at :515-635 render these
docs).

**Fix:** use the crate-qualified path — ``[`mnemo::agent::AgentLoop::set_explicit_provider`]``
— or plain prose.

### L3 — PLAN.md doc-sync omission: the picker-over-routing priority is undocumented

`PLAN.md:269-276` still describes the model policy as "the status-bar picker switches only
the active agent's model … console `/model` and `save_endpoints` stay global". It never
mentions the per-state `[models.*]` routing chain, and now silently omits the load-bearing
new fact this change introduces: **a picked model pins that agent and beats per-state
routing until switched again** (the exact sentence README.md:48 gained). The project
constitution's documentation-sync check covers PLAN.md's technical decisions, and this
change alters the model-routing decision.

**Fix:** extend the PLAN.md:270-274 sentence with the pin semantics, mirroring the README
bullet.

---

## NITS (no action strictly required, but cheap)

- **N1 — inaccurate doc phrase:** the `explicit_provider` field doc (`loop_impl.rs:151-152`)
  describes the pin setter as "`set_model` with an `agent_id` — the GUI status bar / the
  console's per-agent switch". There is no console per-agent switch: the console `/model`
  REPL routes through the global `swap_provider_into_loops` (`src-tauri/src/console.rs:953`)
  by design. Drop "/ the console's per-agent switch" (or reword to "the GUI status bar's
  per-agent switch").
- **N2 — informational:** the pin is intentionally never cleared, so per-state routing is
  permanently disabled for a picked agent for the rest of its lifetime — including states
  entered long after the pick. That is the documented semantic and the README states it;
  noted only so the "until switched again" wording is understood to be literal.

---

**Tests:** the parent reports root `cargo test` 1293 passed / 0 failed and src-tauri
`cargo test` 143 passed / 0 failed (unpiped, exit=0). As a read-only reviewer I did not
re-run them; the compile-sensitive details of the new test were instead verified by hand
against the source (see "Verified correct").
