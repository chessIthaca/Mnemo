# Job 6 — App unification on the headless Engine (IpcState Engine + console rebuild)

Master spec `.coding/specs/headless-free-agent.md` §5.9.10 (:740-753), §7 row 34
(:889-892), §8 (:895-906). The item: "the mnemo app is rebuilt on the public
API — IpcState holds an Engine; every IPC command + the console REPL go
through Engine/AgentSession/Setup/*Api."

This is the follow-up split from job 5 D5 (the master's :770-772 stop-split
rule, executed in job-05's §F 5.5, tree 5d97bc1; filed as backlog
`51f3be28-c726-4599-a16a-c3364465b34d`). Branch: `wt/stream-plain` (the
per-directory working line — the arc's old `wt/mnemo` is retired post-PR-#84,
merged at 4f39f06 2026-10-15). Job 5 closed on the lattice + example +
boundary + acceptance sweep; the G10 acceptance (§7 row 34) does NOT block on
this job — its three legs (no-default build, boundary test, GUI + console
behave as before) stand.

## B. Probe facts (2026-10-16, tree = 43e0804 on wt/stream-plain)

1. **The second-brain problem** (verified): `Engine::brain()` is `pub(crate)`
   (src/headless/engine.rs:138). EngineBuilder's existing seams — `.provider`
   (:101), `.memory_store` (:113), `.shared_resources` (:123) — thread through
   `start_inner` (:129-131). An Engine built app-side constructs its OWN brain;
   the GUI's live config Arc (`brain.config`, the tokio-Mutex the classifier /
   rewire legs flip, read via `block_on` at src-tauri/src/main.rs:462) is NOT
   reachable from an Engine today.
2. **The GUI Ready branch** (verified): `build_brain` at src-tauri/src/main.rs:393;
   the handles at :459-476 — `brain.config` (block_on :462), `attach_laya`
   (:465), `AgentManager::new(256)` + the fan-in receiver + the manager /
   agent_loops Arcs (:468-476). `BrainOutcome` lives at src/host/brain.rs:131-138;
   `Ready` carries the brain **boxed** (proven by the console's deref:
   src-tauri/src/console.rs:1181 `*brain`). Box<Brain> → Arc<Brain> (`Arc::from`)
   is therefore the sharing move — both deref identically, so every existing
   `brain.X` field access is unchanged.
3. **IpcState** (verified): src-tauri/src/ipc/state.rs:47; `AgentRuntimeContext`
   holds manager + loops + factory (factory Option at :112, getter at :305); the
   `app_policy_factory: Option<Arc<…>>` field (:52) is the Option-field precedent
   for the new `pub engine`.
4. **The console** (verified): `run_console` at src-tauri/src/console.rs:1179
   builds its own brain (`crate::build_brain(None, …)` :1180), wires the
   app-side policy factory (:1194-1216 — `WorkflowPolicyFactory` from
   `brain.factory` handles + skills at :1206-1213), `attach_laya` (:1218), the
   pending embedding-model load (:1225-1231). `ConsoleRuntime` (struct :604-621,
   impl :623-853) wraps `mnemo::headless::HostRuntime` (import :38 — already the
   re-export, non-test). **The backlog's :1497-1501 / :2119-2126 anchors are ALL
   `#[cfg(test)]`** (the tests mod opens at :1491; `make_factory` :2119-2126 is a
   test fixture) — boundary-EXEMPT. The non-test wiring is :38 / :1180 /
   :1194-1216 / :604-853.
5. **The console is its own process** (`mnemo console` — no concurrent GUI). Its
   brain is process-local and unique: the console's unification is the API PATH
   (Engine/AgentSession/Setup/*Api), not GUI brain-sharing. One seam, two
   consumers — the GUI shares the GUI's brain, the console shares its own.
6. **The headless surface it consumes** (job-3a/4/5 landed; re-exports at
   src/headless/mod.rs:24-56 — "headless defines no data of its own"):
   `engine.spawn(SpawnOptions)` → AgentSession; `Setup::for_engine`
   (src/headless/setup.rs:499-524; live-apply test
   `patch_settings_live_applies_through_for_engine` :1583-1620 — it Arc-clones
   the engine's shared handles, per the `brain()` doc at engine.rs:135-137);
   `memory()`/`codegraph()` accessors; EngineStats. `EngineOptions`
   (src/headless/types.rs:60-98): workspace {root, state_dir, require_git},
   config_dir, permissions, memory backend (overridden by `.memory_store`),
   index_codegraph, require_git, auto_install_embedder.
7. **The boundary rule**: §5.9.10 bans only `agent::`/`runtime::`/`host::`
   internals; `mnemo::app::*` is sanctioned; `#[cfg(test)]` is EXEMPT.
   tests/boundary.rs::tauri_uses_only_public_api is green at 43e0804. The `Brain`
   type the seam needs gets a headless-facade re-export (`mnemo::headless::Brain`)
   — the established pattern (`SharedResources` is re-exported the same way for
   `.shared_resources`, engine.rs:123).
8. **Job-5's constraint** (§F 5.5 :360-362): "Landing half of it (an Engine
   handle with no consumer) would add dead surface — a certain review finding."
   The proving consumer is load-bearing.## D. Design decision + deliverables

### D1 — the brain-injection seam (DECISION: injection, not two-lane)

- `EngineBuilder::brain(Arc<Brain>)`, pub, mirroring `.memory_store` /
  `.shared_resources` (engine.rs:113/:123); `Brain` re-exported through the
  headless facade (`mnemo::headless::Brain` — the `SharedResources` precedent,
  engine.rs:123). `start_inner` threads `Option<Arc<Brain>>`: `None` → build
  (unchanged); `Some` → adopt — no second brain, no second memory store, no
  second embedder load; `engine.memory()`/`codegraph()`/`brain()` read the
  injected instance.
- Two-lane REJECTED: it contradicts §5.9.10 :747-753 (every command through
  Engine) and "GUI + console behave as before" — a second brain means a second
  `config` Arc, so the GUI's classifier/rewire legs would flip the wrong one:
  exactly the regression the stop-split blocked on (§F 5.5 blocker 1).
- Tests (Rust): an Engine built with an injected brain reports the SAME Arc
  identity through its accessors (memory()/codegraph()); the None-path is
  unchanged (existing tests green); `cargo build -p mnemo
  --no-default-features --features kernel,headless` stays green (the seam is
  headless-surface, no app deps).

### D2 — the Engine construction recipe (ONE recipe, both consumers)

- GUI (main.rs :459-476): `Box<Brain>` → `Arc<Brain>` (`Arc::from` — every
  `brain.X` access is unchanged through deref); the GUI keeps one Arc, the
  clone goes to `EngineBuilder::brain`. EngineOptions: workspace from
  `brain.workspace` {root, state_dir, require_git}; config_dir: the app's;
  permissions per-app; memory overridden via `.memory_store` (the
  `brain.own_memory_store` `Arc<dyn MemoryStoreTrait>` coercion);
  index_codegraph per-app; auto_install_embedder: false (the GUI owns the
  deferred embedder load, main.rs :1225-style). Whether `.provider` /
  `.shared_resources` are also required alongside an injected brain — verify
  `start_inner`'s resolution at execution and record it in the landing note.
- IpcState (state.rs:47): `pub engine: Option<mnemo::headless::Engine>` + a
  getter; None on the non-Ready arms (the `app_policy_factory` Option
  precedent, :52). The Engine is HELD at D2 — no command routing changes yet.
- Console (console.rs :1179-1234): the same recipe on its process-local brain
  — one construction path for both consumers.

### D3 — the proving consumer (DECISION: the Setup::for_engine live-apply)

- The GUI's classifier live-apply (src-tauri/src/ipc/laya.rs) routes through
  the Engine: `Setup::for_engine` (setup.rs:499-524) Arc-clones the engine's
  shared handles, so a settings patch lands in the ONE brain the GUI reads
  (`brain.config`, the block_on at main.rs:462). A wrong-brain Engine fails
  visibly — the GUI would miss the classifier change — proving the brain-share.
  An engine-status command (`engine.stats()`) proves only possession; it stays
  a stretch only if laya.rs's current route turns out to duplicate an
  already-wired engine path (verify at execution).
- Test: the live-apply through the Engine is observable in the GUI-visible
  config (the IPC-contract fixture pattern).

### D4 — the console slice plan (battery per slice = cargo test --workspace —
the 83 integration fixtures + the console fixtures + tests/boundary.rs — plus
the row-35 guard)

- **Slice 1 — the Engine adoption:** `run_console` constructs the Engine via
  D2; `ConsoleRuntime`'s internals (:604-853) swap the direct HostRuntime
  wiring for engine-session spawns (`engine.spawn(SpawnOptions)`); the
  policy-factory / laya / embedder wiring (:1194-1231) keeps operating on the
  same (now-shared) brain. Battery green → commit.
- **Slice 2 — the REPL command surface:** the `/model` + `/reasoning` swaps
  and the live-apply legs through the Engine's session/Setup APIs (keeping
  the console's GLOBAL semantics — the documented divergence, console.rs
  :20-27; the engine-level swap replaces the legacy global switch path).
  Battery green → commit.
- **Slice 3 — shutdown/drain + dead-wiring deletion:** `begin_shutdown`
  (:1164-1170) through the engine session cancel paths; delete the retired
  direct wiring; the boundary scan green end-to-end. Battery green → commit.
- Stop-split guard: any slice whose delta exceeds ~3k lines splits further
  BEFORE review (master :770-772).

## E. Deviations (recorded)

1. The backlog's console anchors :1497-1501 / :2119-2126 are `#[cfg(test)]`-only
   (verified, §B fact 4) — the slice plan targets the non-test wiring (:38 /
   :1180 / :1194-1216 / :604-853); the test fixtures (`make_factory`
   :2119-2126, the AgentHandle tests :2203-2429) are boundary-EXEMPT and stay.
2. Branch: `wt/stream-plain` (the per-directory working line), not the arc's
   retired `wt/mnemo` — the branch policy (one wt/* per agent directory,
   reused across plans).

## F. Acceptance

1. tests/boundary.rs::tauri_uses_only_public_api green — no
   `agent::`/`runtime::`/`host::` paths in src-tauri non-test code; the seam's
   new type (`mnemo::headless::Brain`) facade-covered.
2. cargo test --workspace exit 0 (the app crate compiles against the public
   API only).
3. The 83 integration fixtures + the IPC contract fixtures green; the row-35
   guard (2a goldens) green.
4. GUI + console behave as before — no user-visible change; the Engine is the
   path, not a feature.
5. The D1 seam tests green (injected-brain Arc identity; None-path unchanged)
   + `cargo build -p mnemo --no-default-features --features kernel,headless`
   green.
6. The D3 consumer test green: the classifier live-apply through the Engine
   lands in the GUI-visible config.