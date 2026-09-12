# Deep Architecture Review — 2026-04-18 (HEAD)

Scope: whole-codebase architecture re-review. Baseline taken as given (per prior reviews 2026-09-15, 2026-04-08 five-way): three-layer split SOUND, dependency direction clean, AgentLoopFactory a strength, IPC module split clean, spawn_agent_shared single spawn path a strength, fan-in/forwarder/TurnResolveLatch well-reasoned. This review looks for **drift, gaps, and what those reviews missed**. All claims verified by reading code at HEAD.

---

## VERDICT: SOUND, WITH DRIFT

The three-layer split holds at HEAD. The lib crate still has **zero** Tauri/IPC dependencies (verified: no `tauri::` import in any `src/` module; the `channels.rs` serde mirrors remain the one accepted exception, `src/runtime/channels.rs:249-322`). Dependency direction is unchanged. But the adapter layer (`src-tauri/src/ipc/`) has accumulated **four domain residents** that the prior reviews' "IPC module split clean" conclusion no longer covers, and there are two genuine gaps the prior reviews missed: a **blocking-git-in-forwarder stall** and a **shared `plans_dir` multi-writer exposure** for parentless (UI-spawned) agents. Neither is fatal today; both are cheap to fix now and expensive later.

---

## FINDINGS (ranked)

### A1 — HIGH — The event forwarder runs blocking `git` subprocesses, stalling ALL agent event delivery
`src-tauri/src/ipc/run_all.rs:461-521, 568-617` — `run_all_dispatch_next` and `on_main_turn_resolved` execute `std::process::Command` (git status/add/commit/reset) **synchronously inside the forwarder task** (`events.rs:169`'s single `rx.recv()` loop calls `on_main_turn_resolved` at `events.rs:268-273, 298-307`). Three compounding problems:

1. **Blocking-in-async**: a multi-second `git add -A && git commit` (large repo, Windows process spawn + AV scanning) blocks a tokio worker with no `spawn_blocking`.
2. **Single-consumer stall**: the forwarder is the *only* fan-in consumer. While it commits item N, every other agent — including live UI-spawned agents streaming `TextDelta`s — has its events queued behind the git call. Cap is 256 (`main.rs:113`); agents then block on `fanin_tx.send().await` (`src/runtime/agent.rs:104`), i.e. **agents stop generating** until git finishes. Backpressure propagates all the way into the LLM streams.
3. **Lock held across the subprocess**: `run_all.rs:472-475` runs `checkpoint()` (git) **while holding the `state.project.root` tokio mutex** — any IPC command taking the project lock (`files.rs`, `get_git_branch`) blocks for the same duration. Note the resolution path (`run_all.rs:568`) correctly clones the root and drops the lock first — the two paths are inconsistent.

**Fix (R5 below)**: wrap git calls in `tauri::async_runtime::spawn_blocking`, and clone `project.root` before `checkpoint()` exactly as `:568` does. ~15 LOC, low risk.

### A2 — HIGH — Shared `plans_dir`: every agent loads the same `stack.json`, and parentless agents can all write it
`src/agent/factory.rs:327-328` — `build_inner` does `Workflow::new(self.plans_dir.clone())` + `wf.load_latest()` for **every** agent. Consequences:

- **State inheritance**: a subagent spawned mid-plan starts its in-memory `Workflow` in `Executing` (loaded from `stack.json`). Mitigated for subagents by `set_plan_mutations_allowed(false)` (`spawn.rs:121-131`, enforced at `src/agent/dispatch.rs:114-126`), so only the main agent mutates plans in practice.
- **The gap**: **UI-spawned agents** (`spawn.rs:33-70`, `parent_id: None`) keep `plan_mutations_allowed == true` and share the same `plans_dir`. Spawn agent A → it creates plan P1 → `stack.json = [P1]`. Spawn agent B (loaded a stale copy before P1) → creates P2 → `stack.json = [P2]`. A then calls `update_plan` → persists `[P1]`, **clobbering P2**. Last-writer-wins on the whole stack array; on restart `load_latest` sees only the survivor. Two parentless agents = two uncoordinated writers to `.coding/plans/stack.json`.
- Related: `AgentManager::main_agent_id()` = *min id among parentless agents* (`src/runtime/mod.rs:127-133`). Backlog prompts always target agent 1 (main spawns first, `main.rs:153-166`), but nothing prevents the main agent from exiting (`cancel`), after which a UI-spawned agent silently becomes the backlog dispatch target — probably not what the user expects from "agent 3 in the sidebar".

**Fix (R6)**: route non-main spawns to a per-agent `plans_dir` subdir (`plans_dir.join(format!("agents/{id}"))`, ~25 LOC in `build_with_id`/`spawn_agent_shared`), or minimally document + enforce the single-plan-writer invariant by denying plan mutations for all spawns except agent id 1. Medium risk (changes UI-spawn semantics: they'd no longer resume the main plan).

### A3 — MEDIUM — The "event forwarder" is now an orchestration brain (drift from its charter)
`src-tauri/src/ipc/events.rs` (747 lines). The module doc (`:1-6`) still describes it as the "Rust → frontend bridge," but it now owns: run-all/auto-feed turn-resolution policy (`TurnResolveLatch`, `:59-134`), parent-completion notification + suggestion-text construction (`:419-494`), inactive-subagent cleanup policy on Complete→Executing (`:513-557`), and per-agent workflow-state tracking (`:159-196`). This is **domain policy in the adapter**, and it is the policy least covered by tests: the latch and the suggestion text are unit-tested (`:587-706`), but the arms that wire them — `Started`/`Error`/`Finished`/`Exited` running-flag flips, `notify_parent_on_completion`'s lock dance, `cleanup_inactive_subagents`' FIFO ordering argument (`:532-539`) — have **zero tests** because they need an `AppHandle` and a live `IpcState`. ~90 of 747 lines tested.

The lock discipline inside these arms is correct today (see map below) but is enforced only by comments. Extracting a lib-side `EventCoordinator` fed by the channel and emitting through a trait (`EventSink`) would make the untested 85% testable and shrink `events.rs` to emit-glue. See R2.

### A4 — MEDIUM — `BacklogStore` and run-all git ops are domain code living in the adapter
- `src-tauri/src/ipc/backlog.rs` (527 lines): a persistent, atomically-written domain store with **no Tauri dependency at all**. It belongs in the lib (`myharness::backlog`); today the lib cannot express "a backlog of prompts" even though backlog semantics (main-agent-only dispatch, statuses) are brain-level policy.
- `src-tauri/src/ipc/run_all.rs:103-184`: `git()` / `checkpoint` / `commit_success` / `rollback` are project-domain operations (they are at least well tested via `TestRepo`, `:186-396`).

Both are pure file moves with no behavior change (R1). Net LOC ≈ 0; they become importable by the lib (e.g. a future headless run-all) and testable without `tauri::test` machinery.

### A5 — MEDIUM — Settings commands contain domain validation + config assembly (settings.rs 1746 lines)
`src-tauri/src/ipc/settings.rs:189-399` (`save_endpoints`) and `:963-1260` (`save_settings`) implement: endpoint/model validation policy (`:206-279`), the `[models]` double-`Option` patch semantics (`:818-881, 1153-1182`), pricing/markdown patching, provider rebuild + live swap (`:330-381`). The wire structs (`:418-632`) are correctly adapter-owned and fixture-locked — that part is right. But the *validation rules* (summarize range 0.05–0.95, theme enum, endpoint uniqueness, sentinel `"hash"` handling `:1129-1140`) are domain policy that the lib's `Config` module should own, which would also make them unit-testable without `State<IpcState>` (today only the DTO parsers are tested, `:1262-1399`). See R4.

### A6 — LOW/MEDIUM — Duplicated re-embed fingerprint logic (main.rs vs rewire.rs)
`src-tauri/src/main.rs:741-772` and `src-tauri/src/ipc/rewire.rs:58-92` carry a **near-verbatim duplicate** of the stored-fingerprint check + background `reembed_all` (same comments, same `fps.len() != 1 || fps[0].0 != expected_model || ...` predicate). One will drift. Extract `maybe_reembed(store, embedder, status)` into the lib or `rewire.rs` and call from both. ~−60 LOC, trivial risk (R3).

### A7 — LOW — Startup pull pattern is hand-rolled per surface; the emit-race class is only partially closed
The ContextUsage race was fixed twice-over (registration-time emit `spawn.rs:171-188` + `context_caps` pull `agent.rs:399-406`, correctly **max-only** so a late pull can't clobber a live `used`). Auditing the same race elsewhere:

- `embedder://status` — emitted at setup (`main.rs:230`) before the webview exists; frontend **polls** `get_embedder_status` (App.tsx) → covered.
- `backlog://changed` — initial `backlogList()` pull (`useAgentEvents.ts:301-303`) → covered.
- `agent://event` — module-singleton listener (`useAgentEvents.ts:94-103`), created at first hook mount; the main agent's first `Started` only ever follows user input → covered.
- **Residual gaps**: (1) `App.tsx:188-198` pulls `getWorkflowState` for the **active agent only** — other registered agents' `workflowStates` stay stale until their next transition (a UI-spawned agent that resumed `Executing` from disk shows nothing until it transitions). (2) Direct `app.emit` calls from command contexts (`PromptDispatched` `spawn.rs:425`, `SkillStarted` `agent.rs:671`) race the fan-in path on the *same channel* from *different tasks* — ordering is by construction today (prompt sent before emit), but nothing guarantees it.

A single `startup_snapshot()` command returning `{agents, context_caps, workflow_states, backlog, embedder_status, config}` would replace the five sequential pulls in `App.tsx:166-233` and close the class (R7, ~60 LOC backend, −25 frontend).

### A8 — LOW — Frontend does not re-implement backend-owned state (verified)
Checked each candidate: workflow mirror is event-driven with a startup pull (no logic duplicated — the reducer only records `WorkflowState` strings); backlog is full-list push (`BacklogChangedPayload` carries all items, `backlog_cmds.rs:39-58`) so the frontend holds no derived policy; safety mode reads the runtime lock (`settings.rs:711-720`), which is the documented source of truth (the config.toml copy is deliberately a session-stale persisted default); model display prefers the per-turn resolved model (`agent.rs:376-379`) matching resolution semantics. The one duplication is `ConfigModelResolver`'s own `Arc<RwLock<Config>>` (`main.rs:829`) vs `state.project.config` — two in-memory config copies, but both save paths (`settings.rs:316-321, 1221-1226`) reload + push to the resolver (`sync_model_resolver`), so they cannot diverge through any existing write path. Acceptable; worth a comment in `state.rs`.

### A9 — LOW — `on_main_turn_resolved` swallows the next-dispatch error
`run_all.rs:632` — `let _ = run_all_dispatch_next(...)`: the failure path inside `dispatch_next` already marks the item failed and clears the run state (`:476-489`), so the swallow is mostly benign, but a failure to *send the prompt* (`:511-514` returns `Err`) leaves the run cleared and the item `InFlight` with no note. Minor; fold into R1's move.

---

## LOCK-ORDER MAP (verified at every acquisition site)

Documented invariant (`state.rs:39-44`): **manager → agent_loops**, or (preferred) snapshot under one, drop, read under the other. Never `agent_loops → manager`.

**Actual order at HEAD, verified site-by-site** (M = manager tokio mutex, L = `agent_loops` tokio mutex, W = a `Workflow` tokio mutex, ● = nested/held simultaneously, ○ = sequential, previous dropped):

| Site | Sequence | Complies? |
|---|---|---|
| `agent.rs:359→372` `list_agents` | M ○ L (snapshot-then-drop, documented pattern) | ✅ model |
| `agent.rs:531→536` `get_workflow_state` | L ● W | ✅ (W always innermost) |
| `agent.rs:591→596` `get_plan` | L ● W → drop both | ✅ |
| `agent.rs:645→653→664` `enter_skill` | L ○ (drop `:650`) W ○ W | ✅ explicitly avoids nesting |
| `agent.rs:201-273, 682` control cmds | M only | ✅ |
| `agent.rs:507-515` `set_model` | L ● (loop-internal std locks via `set_provider`) | ✅ see note 1 |
| `events.rs:243, 251, 284, 369, 427, 461, 544, 574` | M only, ≤1 lock per arm, dropped before `run_all` awaits | ✅ per `:229-238` |
| `events.rs:329→332` `Exited` | M ○ L | ✅ |
| `events.rs:427 ○ 450 ○ 461` `notify_parent_on_completion` | M ○ L ○ M (re-acquire) | ✅ no nesting |
| `spawn.rs:95-98` `next_id` | M | ✅ |
| `spawn.rs:107` cleanup | M (held across `try_send`s — deliberate, `:532-539`) | ✅ |
| `spawn.rs:125, 146` subagent setup | W (new agent's own, uncontended) | ✅ |
| `spawn.rs:155-165` register | M (no L inside; fan-in send moved **outside**, `:171-188`) | ✅ this was the deadlock, patched |
| `spawn.rs:192, 196-201` | L ○ M | ✅ |
| `spawn.rs:273→277` allowlist | L ○ (clone Arc, drop) W(parent) | ✅ |
| `run_all.rs:90→94→99` | M ○ L ● W | ✅ |
| `backlog_cmds.rs:249-264` | M only | ✅ |
| `settings.rs:370-373` | L only | ✅ |
| `src/agent/dispatch.rs:145-158` descendant gate | W dropped before tracker → M (`:140-144` comment; code confirms the ToolFilter block closes first) | ✅ see note 2 |

**No `L → M` or `W → M/L` edge exists anywhere.** The invariant holds, and the two risky spots are commented at the site. Notes: (1) loop-internal `std` locks (provider `RwLock`, `resolved_model` mutex) are always taken *under* L and never from a path holding them toward L — the agent task itself touches neither M nor L. (2) The dispatch gate is the one place where lib code awaits into adapter state (via `DescendantTracker`); the seam is clean but is the single most delicate ordering in the codebase — any future edit that keeps W held across `:158` deadlocks against `run_all.rs:94-99`. Recommend a `debug_assert!`-style comment or a wrapper type making the "no W across tracker" structural.

---

## EXTENSIBILITY: TOUCH-POINT COUNTS

| Scenario | Mandatory touch-points | Files | Assessment |
|---|---|---|---|
| **(a) New tool** (existing category/approval path) | **4** — new `src/tool/agent/x.rs`; `tool/agent/mod.rs`; `tool/mod.rs` re-export; `factory.rs::build_registry` register + import | 4 Rust | Good. Rises to 6–8 if it needs a new `ToolCategory` (the `ToolFilter::allows` state matrix fans out) or reviewer visibility (`spawn.rs REVIEWER_BASE_TOOLS`) or a custom approval card (`ApprovalPrompt.tsx`). |
| **(b) New IPC command** | **4** — `#[tauri::command]` fn; `main.rs::generate_handler` entry (the 70+-line manual registry, `main.rs:374-448`); `frontend/src/lib/tauri.ts` wrapper; `types.ts` | 2 Rust + 2 TS | Acceptable. The manual `generate_handler` list is the amplifier; Tauri offers no inventory macro, so leave it but note it doubles as the command census. |
| **(c) New agent event kind** | **8 mandatory, up to 12** — Rust: `AgentEvent` variant, `SerializableAgentEvent` variant, `into_serializable` arm (all `channels.rs`), `events.rs` forwarder arm if lifecycle-relevant; TS: `types.ts` union, reducer fn + `applyAgentEvent` case (`agentEventReducer.ts:757-776`), rendering if transcript-visible (`Message.tsx`), buffering branch only if delta-class (`useAgentEvents.ts:200-224`); fixtures: JSON + `contract_fixtures.rs` + `ipc-contract.test.ts` | 3 Rust + 3–5 TS + 3 fixture | **Highest friction** — but the fixture trip-wire is exactly what makes it safe; the friction is *audited* friction. The three-variant mirror (`AgentEvent`/`Serializable`/`into_serializable`) is the accepted serde-exception cost. A `macro_rules!` that derives the mirror arm would cut 2 points but hurt readability of the one file the exception lives in — not recommended. |
| **(d) New right-panel view** | **2–3** — component file; `rightPanelViews.ts` registry entry (id/label/icon); `disabledTabs` default if off-by-default | 2–3 TS | **Lowest friction. The registry is the pattern the other three should converge toward.** |

The reducer switch is exhaustive against the 19 `SerializableAgentEvent` variants (verified: `agentEventReducer.ts:758-776` ↔ `channels.rs:255-322`), so an added Rust variant *will* fail TS compilation if the reducer isn't extended — a second, compile-time trip-wire beyond the fixtures. Good.

---

## STRUCTURAL RECOMMENDATIONS (ranked by value/risk)

- **R1 — Move `BacklogStore` → `myharness::backlog`, run-all git ops → `myharness::project::git_ops`.** Pure file moves, tests travel with them. **ΔLOC ≈ 0**, risk **low**. Kills A4, makes A9 fixable in testable code, and lets a future headless run-all reuse the checkpoint logic.
- **R5 — `spawn_blocking` for git + clone root before `checkpoint()`.** **ΔLOC +15**, risk **low**. Kills A1 (the only finding with user-visible stalls). Do this first.
- **R3 — Dedup the re-embed fingerprint block** (`main.rs:741-772` ↔ `rewire.rs:58-92`) into one helper. **ΔLOC −60**, risk **trivial**.
- **R7 — `startup_snapshot()` command** returning agents + caps + all workflow states + backlog + embedder status; replaces the 5 sequential pulls in `App.tsx:166-233`. **ΔLOC +60/−25**, risk **low**. Closes A7's residual gaps and generalizes the pull pattern that ContextUsage pioneered.
- **R4 — Extract settings validation/patch application into `myharness::config::patch`.** ~250 LOC moved out of `settings.rs`; command bodies become thin. **ΔLOC ≈ −40 net**, risk **low–medium** (fixture tests must stay byte-identical). Makes A5's policy unit-testable.
- **R2 — Extract forwarder orchestration into a lib-side `EventCoordinator` + `EventSink` trait** (latch, notify-parent, cleanup, running-flag bookkeeping behind a plain channel + trait; `events.rs` keeps only `app.emit`). **ΔLOC +50 net** (~300 moved, ~50 seam), risk **medium** (async refactor of the hottest loop). Unlocks tests for the untested ~85% of `events.rs`. Do **after** R5 — the stall fix is orthogonal and cheaper.
- **R6 — Decide the parentless-agent plan-writer story**: per-agent `plans_dir` subdir for non-main spawns (~25 LOC) **or** an explicit documented invariant + `plan_mutations_allowed = (id == 1)`. **ΔLOC +25 or +5**, risk **medium** (changes UI-spawn resume semantics). At minimum, document that concurrent UI agents clobber `stack.json` (A2) — today it is nowhere stated.

## TEST-ARCHITECTURE ASSESSMENT

Structurally thin: (1) **forwarder arms** — only latch + suggestion text tested; the wiring (running flags, Exited cleanup, notify-parent, cleanup-FIFO) is untested and currently untestable without `AppHandle` (R2 fixes); (2) **run-all orchestration** — git ops are well tested (`TestRepo`), `on_main_turn_resolved`/`halt_run_all_for_approval` state machines are not (needs `IpcState`; R1+R2 fix); (3) **IPC command bodies** — settings DTO parsers tested, command validation paths are not (R4 fixes); (4) frontend — genuinely strong: table-driven per-kind reducer suite + the fixture contract loop on both sides of the boundary. **None of the proposed refactors hurt tests**; R1/R2/R4 each convert currently-untestable code into unit-testable code, and the fixture suites (`.coding/plans/36fa0b20…` H4 work) survive R4 untouched because the wire structs stay in the adapter.

**Priority order: R5 → R3 → R1 → R7 → R4 → R2 → R6.**
