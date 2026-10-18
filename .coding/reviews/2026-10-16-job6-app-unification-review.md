## Verdict: FINDINGS (1 high, 2 low)

The seam, the console rebuild, and the boundary compliance are well-built and verified — but the GUI engine's construction silently re-points the shared brain factory's spawner at the engine's own manager, so background agents started by any post-startup GUI agent become invisible to the GUI. That contradicts the delta's own acceptance ("the GUI untouched in behavior") and is recorded nowhere in the design authority.

### Scope reviewed

Working tree: only `.coding/plans/c903c276.md` modified (step-6 tick + Reviews stamp — accurate bookkeeping, matches shipped state). The review delta is therefore the five job-6 commits on the branch, each read via `git show` plus full-depth code reads of:

- `src/headless/engine.rs` — `EngineBuilder::brain` (:150-153) + exclusivity at `start()` (:178-197); `start_inner`'s injected path (:601-759: skips the config-readiness gate, skips `wire_brain` + provider override + FreePolicyFactory + startup embedder, keeps the host factory wiring); `shutdown` :450, `swap_model` :466, `host` :501, `memory` :474; the seam unit tests :888-962 (Arc::ptr_eq brain identity, memory-accessor mirroring over the brain's store, exclusivity refusal).
- `src/headless/session.rs` — pump responder stashing by `request_id` (:150-199); `SessionCommand::Approve`/`Answer` routing fail-closed on a missing id (:270-314).
- `src/headless/types.rs` (`SessionId::agent_id` :35-41, doc'd) and `mod.rs` (Brain re-export :47).
- `src-tauri/src/main.rs` — `brain_result` consumption; Ready branch `Arc::from(Box)` zero-copy; GUI spawner wiring :502; main agent build :571; engine construction :643-661; the three IpcState sites (:665 `Some`, :854/:952 `None`); `engine_status` registration :1033.
- `src-tauri/src/ipc/state.rs` (:58 field, :326-333 doc'd getter), `agent.rs` (:550-564 `engine_status`), `contract_fixtures.rs` (:527-542 + `assert_fixture` mechanics :54-70).
- `src-tauri/src/console.rs` — imports (:31-62, facade-only), `ConsoleRuntime` (:614-905: `engine.spawn`, `next_event`, SessionCommand answer routing, `swap_model` → `engine.swap_model`, `request_shutdown` via `engine.host()`, `into_engine`), the REPL state machine (:955-1241: SerializableAgentEvent arms, second-approval deny, question-supersede deferral), `begin_shutdown` (:1242), `run_console` (:1256-1464: brain Arc conversion, the ApproveViaApi permission snapshot read before the engine block so the lock never spans the await, the exit handshake, the two-phase exit tail with `engine.shutdown()`).
- `src/host/runtime.rs` — `HostRuntime::start` (:745-781, the `set_spawner`/`set_descendant_tracker` rewiring) and `HostSpawner` (:622-741).
- `src/agent/factory.rs` — the spawner slot semantics (:360, :836-843: "the tool is read per `build`").
- `tests/integration/headless_kernel.rs` :370-432 (the swap fixture, pinning the internal main agent); `tests/boundary.rs` (live `#[test]`, 7 banned tokens incl. alias/grouped forms, string-aware comment + test-item stripping, no per-file exemptions).
- Frontend: `dto-engine-stats.json` (full), `tauri.ts` :302, `useAgentEvents.ts` :519, `ipc-contract.test.ts` imports.
- `PLAN.md` :34-43, `README.md` headless bullets, the spec's D3 landing note.

### H1 (high) — the GUI engine steals the shared factory's spawner: post-startup background agents go invisible

Mechanics, each step verified in code:

1. The GUI wires its `IpcSpawner` (over the GUI's manager/agent_loops) into the shared brain's factory at `main.rs:502`, then builds the main agent at :571-575 — so main keeps the GUI spawner.
2. The engine is constructed **last** (:643-661) over the same `Arc<Brain>` via the seam; `start_inner` passes `brain.factory.clone()` — the very factory the GUI wired — to `HostRuntime::start`, which re-wires `factory.set_spawner(HostSpawner over the ENGINE's manager/agent_loops)` and `set_descendant_tracker` (`src/host/runtime.rs:780-781`).
3. The factory doc is explicit that the slot is read **per build** (`src/agent/factory.rs:836-840`), so every agent built after startup bakes the engine's `HostSpawner` into its `spawn_agent` tool + descendant gate: every runtime `spawn_agent` IPC-command spawn (a live UI flow — `frontend/src/lib/tauri.ts:302`; `useAgentEvents.ts:519` explicitly handles "runtime-spawned" agents) and every tool-spawned background agent.
4. `HostSpawner::spawn_with_parent` registers into the **engine's** manager/agent_loops (`runtime.rs:684-710`) — "IpcSpawner … minus the UI-only event emit" — so the child's events flow only into the engine's pump, which nothing in the GUI consumes (the engine is held; only `engine_status` reads it).

Consequence: a background agent started by any GUI agent other than main is invisible — absent from `list_agents`/`context_caps`, no events to the GUI forwarder, no cancel or per-agent `set_model` reach, no `PromptDispatched` — and it keeps running (LLM turns consuming tokens) until process exit, since the GUI never calls `engine.shutdown()`. Pre-delta the slot was never re-wired, so the identical flow landed GUI-side and was visible. This breaks the plan's acceptance "the GUI + console behave exactly as before". The design authority is silent on the spawner — the job-06 spec, the plan, and the risk list cover only the idle internal main agent; nothing records or accepts this rewiring.

Fix direction (any one suffices): (a) after the engine construction in the Ready branch, re-wire `brain.factory.set_spawner(spawner.clone())` — restoring the host's wiring, which is the seam's own stated philosophy ("the host app's wiring governs"); the held GUI engine spawns no sessions, so nothing of the engine is lost; (b) give `HostRuntime::start`/`start_inner` a preserve-host-spawner mode for injected brains; (c) longer-term, bake the spawner per build from the `SpawnHost` rather than the mutable factory slot. The console is unaffected by any of these — its process-local factory has no competing consumer, and engine-side routing is exactly what the console rebuild wants.

Reviewed-state: 7a3cae6c58d9c29d43a0cd2083f5141e82080e35

### L1 (low) — the dto-engine-stats fixture is pinned Rust-side only; the contract_fixtures module doc overstates the TS-side pin

`contract_fixtures.rs` :13-14 states "The frontend vitest contract suite imports the same fixtures, so a change is caught on the TS side too" — but `ipc-contract.test.ts` does not import `dto-engine-stats.json` (repo-wide, only the spec and the Rust test reference it), and no `EngineStats` TS DTO exists (types.ts untouched — consistent with the deliberately frontend-free design of the proving consumer). Either add the TS import + type mirroring the other `dto-*` fixtures, or annotate this fixture as Rust-pinned until the frontend adopts `engine_status`.

### L2 (low) — PLAN.md's headless-lattice paragraph predates the job-6 surface additions

`PLAN.md` :34-43 ("Kernel/headless feature lattice (2026-10-13, G10)") documents the headless surface + the boundary rule but was not extended for job 6's public additions — `EngineBuilder::brain`, `Engine::swap_model`, `Engine::host` — or for the fact that the app now holds + serves an Engine (`engine_status`) and the console is engine-backed. Module doc comments throughout the delta are exemplary, so this is a one-paragraph refresh, not a rewrite. README's headless bullet (:54) remains accurate as-is.

### Constitution checks

- **Documentation sync** — module docs comprehensively updated; PLAN.md gap → L2; the fixture-doc overstatement → L1.
- **Multi-platform neutrality** — the rebuild is cross-platform (std stdin/stdout, tokio time, no new platform APIs). console.rs's `cfg(windows)` sites are pre-existing (the `attach_console` FFI at :1502 — the sanctioned Windows console-attach for the GUI-subsystem binary, unchanged in content by the delta) or test-gated (:2625+). No new `cfg(windows)`-only code outside the sanctioned gate.
- **File-tools-first** — no shell-based file-mutation patterns in the delta; all source changes read as tool-mediated edits.
- **Warning-free** — no `#[allow(...)]` anywhere in the delta; the measured green `cargo test --workspace` under `#![deny(warnings)]` covers it.
- **Bookkeeping accuracy** (one line) — the uncommitted plan tick + Reviews stamp match shipped state; the spec's D3 landing note accurately records the twin construction + the idle-agent cost — its "GUI behaves exactly as before" claim is the one item contradicted by H1.

### Acceptance verified (the green items)

- **Boundary (risk e)**: `tauri_uses_only_public_api` is a live `#[test]` (un-ignored since job-5 D4 — the :68 doc line describes history), scans all of src-tauri/src production code with no per-file exemptions (only `#[cfg(test)]` items and comments stripped), 7 banned tokens including the alias/grouped-import forms. The rebuilt console names only the facade (`mnemo::` root, pre-existing `mnemo::config`/`provider`/`tool`, `mnemo::headless`; the lone `mnemo::runtime` use is `cfg(test)`), and the crate-root re-exports it leans on (`SerializableAgentEvent`, `PermissionPolicy`) exist (`src/lib.rs` :58/:62).
- **The seam (risk a)**: exclusivity enforced at `start()` with a clear error (refusal test asserts code `start_failed` + "exclusive"); the injected path skips `wire_brain`, the provider override, FreePolicyFactory, the startup embedder, and the config-readiness gate; the seam tests pin `Arc::ptr_eq` brain identity plus the memory-accessor mirroring over the brain's store. No double-wiring of memory/embedder/policy anywhere in the GUI or console construction.
- **Console (risks b, c, f)**: prompts/approvals/questions route by `request_id` through the engine's pending maps; a second approval while one is pending → auto-Deny, fail-closed, the first stays answerable; a superseding question → the NEW one gets the Freeform deferral answer, the first stays pending; the session side fails closed on a missing id. Exit: `begin_shutdown` cancels every agent through `engine.host()` with the pump alive so `Exited` events are observed, bounded by the 5s drain deadline, then `into_engine().shutdown()` consumes the engine — no double-cancel hazard (`Cancel` sends are `let _`-ignored; the abort is single-shot), no leak (the pump aborts; the brain's Arc drops at scope end; browser + trace teardown follow). The `ApproveViaApi` snapshot is read under the config lock BEFORE the engine block, so the lock never spans the async start (risk f clean).
- **Swap (risk d)**: `Engine::swap_model` swaps the factory's provider slot + default-effort record + every live loop, keyed by agent id; the fixture pins that the internal main agent (`HostRuntime::start` spawns one — the documented idle-agent cost) is swapped too. The GUI's per-agent `set_model` path is untouched.
- **GUI construction**: one `Arc::from(Box)` zero-copy conversion; `engine: Some` only in the Ready branch (:665), `None` at :854/:952; `engine_status` registered (:1033) and reads `state.engine()?.stats()` — the proving consumer over the ONE brain store, with the wrong-brain detection rationale documented.

The parent's measured batteries (cargo test --workspace: 3071 lib + 84 integration + 316 app-crate; npm test; zero warnings) are consistent with everything read — the findings above are behavioral/structural, not compile- or test-visible, which is exactly why they need the parent's decision rather than a red CI.