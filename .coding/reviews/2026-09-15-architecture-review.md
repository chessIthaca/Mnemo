# Architecture Review — myharness (2026-09-15)

**Perspective:** System architecture, module boundaries, scalability, coupling.
**Reviewer:** read-only architecture review (conducted in-session; no `spawn_agent` tool available).
**Grounding:** current source (`src/`, `src-tauri/`, `frontend/`), `PLAN.md`, `Cargo.toml`.

---

## Overall verdict: **Sound**

myharness has a genuinely clean three-layer architecture: a Tauri-free Rust brain
(`myharness` lib crate), a thin-intent IPC adapter (`myharness-app` binary), and a
React/Zustand frontend. The dependency direction is correct — `src/lib.rs` exposes
only `agent/app/config/error/memory/model_resolver/project/provider/runtime/
safety_rules/skill/tool/workflow`, none of which import Tauri or serde-IPC concerns.
The IPC bridge correctly lives in the binary crate. The single most important
correctness property — **per-agent workflow independence** — is real:
`AgentLoopFactory` builds each agent its own `Workflow` + `ToolRegistry`
(`src/agent/factory.rs`), and the dispatch layer re-checks the `ToolFilter` at
execution time so a hallucinated name omitted from the schema still cannot run
(`src/agent/dispatch.rs:88-107`).

---

## Findings

### A1 — `AgentManager` is a single serialization point (Low)

**What:** `AgentManager` lives behind `Arc<tokio::sync::Mutex<AgentManager>>`
(`src-tauri/src/ipc/spawn.rs:82`, `src-tauri/src/main.rs:80`). Every access —
`send`, `list`, `get`, `set_running`, `parent_id`, `main_agent_id`,
`has_running_descendants` — acquires this same mutex. Even `set_running`, which
uses an `AtomicBool` on the handle (`src/runtime/channels.rs:497`), still locks
the mutex to reach the handle via `self.agents.get(&id)`
(`src/runtime/mod.rs:107-111`).

**Impact:** Under multi-agent concurrency (several subagents running
simultaneously), all manager operations serialize. For a single-user coding
harness with a handful of agents this is acceptable, but it is the one structural
bottleneck that would limit fan-out scale.

**Recommendation direction:** If multi-agent throughput becomes a goal, split the
manager into a `DashMap<AgentId, AgentHandle>` (lock-free reads) + a separate
mutex only for structural mutations (register/remove). The `AtomicBool` running
flag already supports lock-free reads; the HashMap is the only thing forcing the
lock. Low priority for the current single-user scale.

### A2 — `AgentLoop` field count is high but justified (Informational)

**What:** `AgentLoop` carries 13+ fields (`src/agent/loop_impl.rs:46-120`), many
behind `Mutex`/`RwLock`. The `SessionState` extraction (A2 remediation) pulled
`session_id` + `last_prompt_tokens` into a sub-struct, but the loop still holds
`provider`, `context_manager`, `tools`, `workflow`, `sandbox`, `constitution`,
`safety_mode`, `memory`, `safety_rules`, `vision`, `session`, `agent_id`,
`plan_mutations_allowed`, `model_resolver`, `is_subagent`, `fill_rate`,
`forced_model`.

**Assessment:** Each field is individually justified and documented. The
complexity is inherent to an agent loop that supports runtime model swaps,
per-context model resolution, skills, subagent policy, and vision fallback. Not
a defect — noting it so future growth doesn't silently push it past
maintainability. The `ConstitutionHolder` enum (`Source` vs `Static`) is a clean
abstraction for the mtime-checked re-read pattern.

### A3 — `take_fanin_rx` dummy-receiver pattern is correct but subtle (Informational)

**What:** `AgentManager::take_fanin_rx` replaces the real receiver with a dummy
`mpsc::channel(1).1` so `next_event()` on the manager returns `None` immediately
after the forwarder takes ownership (`src/runtime/mod.rs:78-83`). This prevents
deadlock (the forwarder must read events without holding the manager lock).

**Assessment:** Correct and well-documented. The only risk is if someone calls
`next_event()` after `take_fanin_rx()` expecting real events — it silently returns
`None`. The doc comment covers this. No action needed.

### A4 — Channel types carry serde derives (justified concession) (Informational)

**What:** `AgentEvent` / `AgentCommand` in `src/runtime/channels.rs` derive
`Serialize`/`Deserialize` and carry a `SerializableAgentEvent` mirror
(`channels.rs:217-278`) whose only purpose is the IPC boundary. The brain thus
has a serialization concern in its core channel types.

**Assessment:** This is a **justified, minimal** concession — the brain needs
*some* serializable event form to be usable by any UI host, not just Tauri. It
does not make the brain depend on Tauri. The `AgentSpawner` trait
(`src/runtime/mod.rs:183-210`) keeps the spawn capability inverted (the tool
knows only the trait, the IPC layer provides the concrete impl). Decoupling is
real; the serde derives are the cost of a channel contract. No action needed.

### A5 — IPC module split is clean (Strength)

**What:** The IPC layer (`src-tauri/src/ipc/`) is split into domain modules
(agent, settings, files, backlog, backlog_cmds, spawn, events, run_all, state,
approval, questions, contract_fixtures). The `spawn_agent_shared` function
(`src-tauri/src/ipc/spawn.rs:80`) is the single spawn path used by the UI button,
the agent tool, and main-agent startup — eliminating the prior duplicated
spawn-path finding (M3).

**Assessment:** This is a well-factored boundary. Golden JSON contract fixtures
(`contract_fixtures.rs`) + `ipc-contract.test.ts` guard the Rust↔TS event/DTO
shapes. Strong.

---

## Strengths

1. **Dependency direction is clean** — brain imports no Tauri/IPC types.
2. **Per-agent workflow independence** — factory-built, dispatch-rechecked.
3. **Channel contract is faithful** to the PRD (typed `AgentCommand`/`AgentEvent`).
4. **Fan-in / forwarder / dead-agent-cleanup** logic is carefully reasoned and
   tested (`TurnResolveLatch` prevents double-resolution; `cleanup_for_agent` is
   agent-scoped).
5. **Plan-first workflow** uses disk as source of truth (`stack.json` sidecar +
   plan markdown), surviving restarts.
6. **Composition-root honesty** — failed `build_brain` still opens the window
   with a startup-error screen (`src-tauri/src/main.rs:39-44`).

## Remediation order

1. **A1** (Low) — DashMap for the agent registry if multi-agent scale becomes a goal.
2. Everything else is informational/strength — no action required.
