# Architecture Review — Maintainability / Modularity / Cohesion

**Perspective:** Maintainability, modularity, architecture cohesion  
**Reviewer:** read-only architecture agent  
**Date:** 2026-04-08  
**Scope:** `src/` (brain), `src-tauri/src/` (IPC shell), `frontend/src/` (React UI), `PLAN.md`, `agent.md`, tests

---

## Executive summary

The codebase has a **strong three-layer architecture**: a Tauri-free Rust brain (`myharness`), a thin-in-intent IPC adapter (`myharness-app`), and a React/Zustand frontend. The agent↔UI contract (`AgentCommand` / `AgentEvent` / `SerializableAgentEvent`) is the right long-term seam, and core domain logic is unit-testable without the UI.

Maintainability pressure is concentrated in a few **god modules** (`ipc/commands.rs` ~2.2k LOC, `useAgentStore.ts` ~1.5k, `provider/openai.rs` ~1.6k, `memory/mod.rs` ~1.3k) and in **manually mirrored TS/Rust contracts** with no codegen. Multi-agent is “ready” for spawn/fan-in/approvals, but **plan isolation is weaker than PLAN.md suggests**: agents share one `plans_dir` + `stack.json`, so concurrent plan mutations can collide. Splitting the IPC and frontend god files, clarifying multi-agent plan ownership, and hardening the IPC schema boundary would yield the highest maintainability ROI.

---

## Architecture map

```
┌─────────────────────────────────────────────────────────────────────────┐
│  frontend/src  (React 18 + TS + Zustand + Tailwind + Radix)             │
│  App → layout (Sidebar/Main/Right/Status/Input) → views + settings      │
│  hooks/useAgentStore  ·  lib/tauri.ts (invoke/listen)  ·  lib/types.ts  │
└───────────────────────────────▲─────────────────────────────────────────┘
                                │ Tauri IPC
                    commands (FE→Rust) · events (Rust→FE)
┌───────────────────────────────┴─────────────────────────────────────────┐
│  src-tauri/src  (myharness-app)                                         │
│  main.rs (composition root: build_brain, spawn main agent, wire IPC)    │
│  ipc/                                                                   │
│    commands.rs  — 42 #[tauri::command] + IpcSpawner + settings DTOs     │
│    events.rs    — fan-in → SerializableAgentEvent → emit                │
│    approval.rs  — oneshot map keyed (agent_id, tool_call_id)            │
│    backlog.rs / run_all.rs — UI queue + overnight loop                  │
│    state.rs     — IpcState bag                                          │
└───────────────────────────────▲─────────────────────────────────────────┘
                                │ uses myharness as path dependency
┌───────────────────────────────┴─────────────────────────────────────────┐
│  src/  (myharness lib — ZERO tauri imports)                              │
│                                                                         │
│  runtime/  AgentManager · channels · AgentTask · AgentSpawner traits    │
│       ▲ cmd inbox              │ fan-in events                          │
│  agent/  factory → AgentLoop (loop_impl · turn · dispatch · approval ·  │
│          context · prompt)                                              │
│       │ tools via ToolRegistry                                          │
│  tool/   agent/* · workflow/* · memory/*                                │
│  workflow/  plan stack + plan_file (disk SOTs under .coding/plans/)     │
│  provider/  LlmClient · openai · stream · vision · client_factory       │
│  memory/    SQLite+FTS5+vectors · consolidation · embedder              │
│  config/ project/ skill/ safety_rules/ error/ app/                      │
└─────────────────────────────────────────────────────────────────────────┘
```

### How a turn flows

1. UI `invoke("send_prompt")` → IPC pushes `AgentCommand::Prompt` into the agent inbox.  
2. `AgentTask` runs `AgentLoop::run_turn`: constitution + workflow filter → provider stream → tool dispatch/approval → results back into messages.  
3. Events fan in through `AgentManager` → `events.rs` strips oneshots, updates running state / child-finished / backlog hooks → `agent://event`.  
4. Zustand `handleAgentEvent` reduces per-agent transcript + side effects (diff snapshot, plan version bump, etc.).

### Intended vs actual multi-agent ownership

| Concern | Intended (PLAN.md) | Actual |
|---|---|---|
| Process / inbox / events | Per-agent | Per-agent ✅ |
| Workflow object | Per-agent | Per-agent in-memory ✅ |
| Plan files / `stack.json` | Independent plans | **Shared** `plans_dir` + single `stack.json` ⚠️ |
| Memory DB | Shared project store | Shared ✅ (WAL reads) |
| Approvals | Per `(agent_id, tool_call_id)` | Per-agent keys ✅ |
| Provider / safety / config | Global by design | Global ✅ |

---

## Findings by severity

### Critical

_(None that block shipping today, but the next item is the closest to a structural correctness/maintainability landmine.)_

### High

#### H1. `ipc/commands.rs` is a god module (~2178 LOC, 42 commands)

**Where:** `src-tauri/src/ipc/commands.rs` (entire file); registered in `src-tauri/src/main.rs` ~185–226.

**What:** One file owns agent control, safety rules, settings/endpoints/keys, stats, file browsing, conversation save/load, backlog/run-all, skill entry, model swap, **and** `IpcSpawner` / `spawn_agent_shared`. Cross-module callbacks (`on_main_turn_resolved`, `halt_run_all_for_approval`, `emit_prompt_dispatched`) are also parked here, so `events.rs` depends back on `commands`.

**Why it hurts:** Any IPC change risks merge conflicts and accidental coupling. Reviewers cannot load the full surface. New features default to “add another command here.” Composition and domain policy (settings rewire, spawn, backlog resolution) are mixed with pure request handlers.

**Recommendation direction:** Split by domain modules that re-export commands, e.g. `ipc/agent_cmds.rs`, `ipc/settings.rs`, `ipc/files.rs`, `ipc/backlog_cmds.rs`, `ipc/spawn.rs`, keeping `mod.rs` + `generate_handler!` as the registry. Move spawn/runtime wiring next to `state`/`events`, not settings.

#### H2. Multi-agent “independent plan” claim vs shared plan disk SOTs

**Where:**  
- `src/agent/factory.rs` 84–85, 236–246 (`plans_dir` shared; every build `load_latest()`)  
- `src/workflow/mod.rs` 442–475 (`persist_stack` → single `stack.json`)  
- Tests: `factory.rs` 468–541 (`two_builds_produce_independent_workflows` vs `build_loads_existing_plan_from_disk`)  
- `PLAN.md` 194–209 (“independent per-agent plan/workflow”)

**What:** In-memory `Workflow` instances are separate (good), but they all point at the same `.coding/plans/` directory and the same `stack.json`. A newly spawned agent **loads the latest shared stack**. Concurrent agents that call `create_plan` / `complete_step` / `update_plan` race on the same files; last writer wins.

**Why it hurts:** PLAN.md and factory docs oversell isolation. Reviewer agents and the main agent can clobber each other’s plan stack. Future “true multi-agent orchestration” work will keep rediscovering this. Tests encode both stories without documenting the concurrency hazard.

**Recommendation direction:** Decide explicitly:  
(A) **Shared project plan** (document it; serialize workflow mutations; only main agent mutates plans), or  
(B) **Per-agent plan namespaces** (e.g. `.coding/plans/<agent_id>/` or plan id prefixes + per-agent stack sidecars). Update PLAN.md to match.

#### H3. Frontend state god object: `useAgentStore.ts` (~1515 LOC)

**Where:** `frontend/src/hooks/useAgentStore.ts` (appearance helpers ~1–200, `AppState` ~297–510, event reducers ~674–1110, store impl ~1190–end).

**What:** One Zustand store owns:
- per-agent transcripts / streaming / approvals / steers  
- workflow phase map, agent names/parents  
- model/provider/pricing/git branch  
- right-panel chrome + disabled tabs  
- full appearance theme (fonts, 4 UI colors, 7 code colors, localStorage)  
- backlog + run-all  
- settings dialog open state  
- tool-output log + last-diff snapshot  

Event handling was partially factored into per-kind reducers (good), but the file remains a single coupling hub. Almost every layout/view imports it.

**Why it hurts:** UI feature work thrashs one file; appearance prefs have nothing to do with agent event reduction; testing requires pulling the whole store; circular mental model for new contributors.

**Recommendation direction:** Split into focused stores/modules: `agentRuntimeStore` (transcripts/events), `uiChromeStore` (panels/tabs), `appearanceStore`, `backlogStore`, with a thin facade if needed. Keep pure reducers in a separate `agentEventReducer.ts` (already nearly extractable).

#### H4. Manual Rust↔TS contract drift risk (no single schema source)

**Where:**  
- Rust: `src/runtime/channels.rs` (`SerializableAgentEvent`, `AgentCommand`)  
- IPC DTOs: `src-tauri/src/ipc/commands.rs` (`AgentInfo`, `WorkflowStateInfo`, `EndpointDto`, ad-hoc `get_config` JSON ~700–732)  
- TS: `frontend/src/lib/types.ts`, `frontend/src/lib/tauri.ts` (`EndpointInfo`, `AppConfig` live beside wrappers)

**What:** Event kinds, workflow states, backlog shapes, and settings payloads are hand-mirrored. `get_config` / `get_settings` return loosely typed `serde_json::Value` on the Rust side while TS re-declares shapes. `EndpointInfo` sits in `tauri.ts` rather than `types.ts`. PLAN.md still shows older channel sketches (`Prompt(String)`, thinner `AgentEvent` / `Error` without `retrying`).

**Why it hurts:** Adding a field to `SerializableAgentEvent` requires coordinated edits in Rust + TS + reducers + often UI; failures are runtime, not compile-time. This is the #1 long-term cohesion risk across the IPC boundary.

**Recommendation direction:** Prefer one of: `ts-rs` / `specta` / JSON Schema export from Rust, or at least a CI check that golden JSON fixtures round-trip. Keep all FE wire types in one module; stop returning untyped `Value` for stable settings surfaces.

### Medium

#### M1. Provider client is a monolith (`openai.rs` ~1557 LOC)

**Where:** `src/provider/openai.rs`; trait surface in `src/provider/mod.rs`.

**What:** Streaming SSE, request JSON building, tool-call accumulation integration, capabilities, retries/error mapping, and model quirks likely live in one implementation file (largest brain module after tests).

**Why it hurts:** Provider bugfixes and new capabilities (caching headers, vision payloads, alternate SSE shapes) collide. Harder to review than the clean `LlmClient` trait suggests.

**Recommendation direction:** Split along natural seams already implied by the design: `request.rs`, `sse.rs`, `capabilities.rs`, keep `OpenAiClient` as a thin façade. `stream.rs` already exists — lean into that boundary.

#### M2. Memory module size and mixed responsibilities (`memory/mod.rs` ~1275 LOC)

**Where:** `src/memory/mod.rs` plus `consolidation.rs`, `types.rs`, `schema.rs`, `embedder.rs`, `strength.rs`.

**What:** Core store impl, scoring, session/stats queries, and trait object surface are heavy in `mod.rs`. Stats used by the UI (`get_session_stats` / `get_project_stats`) pull through the same module.

**Why it hurts:** Memory retrieval changes risk touching stats/session bookkeeping. Harder onboarding to the four-tier model.

**Recommendation direction:** Extract `store.rs` (CRUD/search), `stats.rs` (session/project aggregates), leave `mod.rs` as the façade + trait re-exports.

#### M3. Composition-root sprawl and duplicated spawn paths

**Where:**  
- `src-tauri/src/main.rs` (~372 LOC) — `build_brain`, main-agent spawn, spawner wiring  
- `src-tauri/src/ipc/commands.rs` 290–413 — `spawn_agent_shared` / `IpcSpawner`  
- Main agent registration is **not** fully identical to `spawn_agent_shared` (setup inlines a similar sequence at `main.rs` ~100–114)

**What:** Two spawn implementations must stay behavior-compatible (id allocation order, parent_id, loop map insert, optional initial prompt, PromptDispatched emit).

**Why it hurts:** Subtle multi-agent bugs when one path is updated (e.g. parent tracking, event emission) and the other is not.

**Recommendation direction:** One `spawn_agent_shared` (or brain-side helper over manager+factory) used by main startup, UI command, and `IpcSpawner`.

#### M4. `IpcState` is an unstructured dependency bag

**Where:** `src-tauri/src/ipc/state.rs` 32–86.

**What:** Manager, loops, factory, memory, project, config, sandbox, approvals, safety, backlog, auto-feed, run-all, single_in_flight — everything commands might need.

**Why it hurts:** Every command takes the world; hard to see which subsystems a command actually needs; encourages more fields instead of sub-contexts. Not wrong for a small app, but it scales poorly with 42 commands.

**Recommendation direction:** Group into nested contexts (`AgentRuntime`, `ProjectContext`, `BacklogContext`) or pass narrower state types per command module.

#### M5. Tool registration is a centralized manual list

**Where:** `src/agent/factory.rs` `build_registry` ~295+.

**What:** Adding a tool requires implementing `Tool` **and** remembering to register it in the factory (and possibly workflow filter / skill allow-lists / safety docs).

**Why it hurts:** Easy to forget registration; factory becomes a churn hotspot as tools grow (already ~15 tools).

**Recommendation direction:** Keep the explicit list (clarity is good) but group registrations (`register_agent_tools`, `register_workflow_tools`, `register_memory_tools`) and add a unit test that expected names are present.

#### M6. Right-panel extensibility is multi-touch

**Where:**  
- `frontend/src/hooks/useAgentStore.ts` `RightPanelTab` + `ALL_RIGHT_PANEL_TABS` ~259–272  
- `frontend/src/components/layout/RightPanel.tsx` `TABS` array + switch/render ~14–23  
- Sidebar enable/disable tooling also references tab ids

**What:** A new view needs union variant, default list, icon/label row, and panel body wiring — easy to miss one.

**Recommendation direction:** Single view registry (`{ id, label, icon, component }[]`) driving store defaults and UI.

#### M7. PLAN.md drift from shipped reality

**Where:** `PLAN.md` status block (~21–34), channel contract sketch (~237–269), terminology; vs current `channels.rs` / skills / backlog / plan stack.

**What:** Doc still frames the work as “UI swap only” and shows incomplete event/command shapes. Skills, backlog/run-all, plan stack, `ParentAwareSpawner`, conversation save/load, etc. are production features not fully reflected in the architecture sketch.

**Why it hurts:** New contributors (and agents) plan against stale seams; reviews argue the wrong invariants (see H2).

**Recommendation direction:** Refresh PLAN.md architecture section as an as-built doc, or add `docs/architecture.md` generated from code-owned comments and link PLAN.md to it.

#### M8. Coarse errors at the IPC boundary

**Where:** `src/error.rs` (typed `Error` enum with string payloads); virtually all commands `Result<T, String>` via `map_err(|e| format!(...))`.

**What:** Brain errors are structured enough for Rust matching, but IPC flattens to unstructured strings. Frontend cannot branch on error codes.

**Why it hurts:** UI error handling stays stringly; i18n and recovery paths stay ad hoc.

**Recommendation direction:** Optional later: stable `{ code, message }` error DTO for user-facing commands.

#### M9. Frontend test coverage is thin relative to store complexity

**Where:** `frontend/src/hooks/useAgentStore.test.ts`, `frontend/src/components/settings/types.test.ts` only (project tests). Brain/IPC side is comparatively rich (~many module tests + `tests/*.rs`).

**What:** Event reducer behavior (tool merge, approval lifecycle, child_finished, backlog apply) is high-churn and under-tested on the TS side.

**Why it hurts:** Refactors of H3 will be scary without locking reducer semantics.

**Recommendation direction:** Extract pure reducers and unit-test every `SerializableAgentEvent` kind (table-driven).

### Low

#### L1. `runtime/agent.rs` is large (~1040 LOC)

Agent task lifecycle + tests in one file. Consider splitting tests or the command-handling loop from bookkeeping.

#### L2. `workflow/mod.rs` (~996 LOC) mixes state machine + persistence + large in-file tests

Already has `plan_file.rs`; stack sidecar / skill overlay logic could be submodules. Tests at bottom dominate file length.

#### L3. Duplicate appearance concerns

Theme/font/color helpers live in the agent store file; CSS tokens also in `globals.css`. Acceptable but noisy.

#### L4. `app` module is nearly empty

`src/app/mod.rs` is only a panic hook after TUI removal — fine, but the module name suggests a larger app layer that no longer exists. Could move hook to `lib.rs` or `util`.

#### L5. Settings DTO dual paths

`EndpointDto` / `PricingDto` / `SettingsSaveDto` in commands vs config domain types in `src/config`. Mapping is explicit (good) but verbose; keep validation in one place (`into_endpoint` is a good pattern — extend it, don’t fork).

#### L6. Public API docs are generally good in the brain; IPC/TS uneven

Brain modules and tools often have solid `//!` and `///` docs (constitution-aligned). Many TS exports rely on short comments; some invoke wrappers are well documented (`interrupt`, `spawnAgent`), others are bare.

---

## Strengths

1. **True UI-decoupled brain** — `src/**` has **zero** `tauri` references; the library can be tested and theoretically reused by another shell. This matches PLAN.md’s best architectural decision and is enforced in practice.

2. **Clear channel contract** — `AgentCommand` / `AgentEvent` / `SerializableAgentEvent` with explicit oneshot extraction (`into_serializable`) is textbook adapter-layer design for approval gating across IPC.

3. **Agent loop modularization** — Split into `loop_impl` / `turn` / `dispatch` / `approval` / `context` / `factory` / `prompt` (see `src/agent/mod.rs`) is a maintainability win versus a single mega-loop.

4. **Tool trait + workflow gate** — `Tool` + `ToolFilter` + `SafetyLevel` / `never_auto_for` give a consistent extension model; skills add file-defined overlays without recompile (`.coding/skills/*.toml`).

5. **Factory pattern for multi-agent construction** — `AgentLoopFactory` documents shared vs per-agent deps carefully; safety mode sharing and provider swap semantics are explicit.

6. **Disk-backed workflow SOTs** — Plans as markdown checklists under `.coding/plans/` with derived in-memory state support crash resilience and human inspection.

7. **Config/project layout clarity** — Global `~/.myharness/` vs per-project `.coding/` + root `agent.md` is easy to explain and match on disk.

8. **Approval isolation** — Per-agent pending approval keys and cleanup on exit show thoughtful multi-agent concurrency design on the IPC side.

9. **Test culture in Rust** — Broad unit coverage across tools, workflow, safety, provider stream parsing, runtime parent/child routing; integration tests under `tests/`.

10. **Composition root honesty** — Failed `build_brain` still opens the window with `startup_error` and a working backlog — practical architecture for self-hosting development.

---

## Top 10 recommended improvements

(Recommendations only — not implemented in this review.)

1. **Split `ipc/commands.rs` by domain** (agent, settings, files, backlog, spawn) and collapse duplicate spawn paths onto one helper.  
2. **Resolve multi-agent plan ownership** (shared+serialized vs per-agent namespaces) and align PLAN.md + factory docs + tests with that decision.  
3. **Break up `useAgentStore.ts`** into runtime / appearance / backlog / chrome stores; extract pure event reducers + table tests.  
4. **Introduce a typed IPC schema pipeline** (codegen or CI golden fixtures) for `SerializableAgentEvent` and major DTOs; eliminate ad-hoc `serde_json::Value` for stable APIs.  
5. **Split `provider/openai.rs`** into request builder / SSE parser / client façade aligned with `stream.rs`.  
6. **Extract memory store vs stats** from `memory/mod.rs` to reduce unrelated churn.  
7. **Add a right-panel view registry** so new tabs are one-entry extensions.  
8. **Group tool registration** in the factory and assert the expected tool name set in a unit test.  
9. **Refresh as-built architecture docs** (PLAN.md or `docs/architecture.md`) including skills, backlog, plan stack, and global-vs-per-agent matrix.  
10. **Harden FE reducer tests and optional structured IPC errors** before the next large UI feature wave.

---

## Open questions for the team

1. **Plan isolation product intent:** Should background agents (reviewers, explorers) ever mutate the project plan stack, or should plan tools be main-agent-only / read-only for children?  
2. **Schema tooling appetite:** Is `specta`/`ts-rs` acceptable in the Tauri build, or is a lighter fixture-based CI check preferred?  
3. **IPC module split timing:** Do it opportunistically during the next settings/backlog feature, or as a dedicated refactor plan?  
4. **Memory scale assumptions:** Is the in-process vector scan + single writer still the target for the next 12 months, or is out-of-process/multi-project memory on the horizon (would push on `MemoryStore` boundaries)?  
5. **Second UI shell?** If the brain must stay shell-agnostic long-term, should spawn/backlog stay in `src-tauri`, or should “app services” move into the lib behind traits (today backlog is IPC-only — which is reasonable)?  
6. **Provider roadmap:** Will non-OpenAI-compatible APIs appear, or is “one OpenAI-compatible client, swap base URL” locked enough that splitting `openai.rs` is “only” maintainability?  
7. **Frontend state library boundaries:** Stay on multi-store Zustand, or introduce a small event-sourced runtime module while keeping Zustand for chrome?

---

## Appendix A — Size hotspots (approx. line counts)

| Lines | Path | Role |
|------:|------|------|
| 2178 | `src-tauri/src/ipc/commands.rs` | IPC god module |
| 1557 | `src/provider/openai.rs` | Provider impl |
| 1515 | `frontend/src/hooks/useAgentStore.ts` | FE state god module |
| 1513 | `src/agent/tests.rs` | Agent loop tests |
| 1275 | `src/memory/mod.rs` | Memory store |
| 1040 | `src/runtime/agent.rs` | Agent task runtime |
| 996 | `src/workflow/mod.rs` | Workflow state machine |
| 735 | `src/tool/agent/git.rs` | Git tool |
| 680 | `src/agent/turn.rs` | Turn driver |
| 593 | `src/tool/mod.rs` | Tool trait/registry |
| 583 | `src/agent/factory.rs` | Per-agent construction |
| 484 | `frontend/src/lib/tauri.ts` | Invoke wrappers |
| 405 | `src-tauri/src/ipc/backlog.rs` | Backlog store |
| 372 | `src-tauri/src/main.rs` | Composition root |

## Appendix B — Extensibility cheat sheet (current friction)

| Extension | Touch points today | Friction |
|---|---|---|
| New agent tool | `tool/agent/<x>.rs`, `tool/agent/mod.rs`, `factory::build_registry`, maybe filter/skill docs | Medium — centralized register |
| New workflow tool | `tool/workflow/*`, factory, `Workflow`/`ToolFilter` | Medium |
| New skill | Drop `.coding/skills/<name>.toml` | **Low** |
| New provider kind | `ProviderKind`/capabilities, `client_factory`, possibly `openai.rs` quirks | Medium–High |
| New workflow state | `WorkflowState`, filters, prompts, FE union, plan persistence | High |
| New right-panel view | Tab union, store defaults, RightPanel, sidebar | Medium (multi-touch) |
| New IPC command | `commands.rs`, `generate_handler!`, `tauri.ts`, maybe `types.ts`, UI call site | Medium–High |
| New AgentEvent kind | `channels.rs`, forwarder, TS union, reducer, UI | High (drift-prone) |

---

*End of report. No source files outside this review path were modified.*
