# Chief Architect Review — myharness (2026 whole-codebase review)

**Reviewer perspective:** overall system architecture and its ability to scale/evolve.
**Scope:** the Rust brain (`src/`), the Tauri app shell + IPC bridge (`src-tauri/`), the React frontend (`frontend/`), and the tests (`tests/`).
**Grounding:** `.coding/reviews/2026-review-baseline.md`. All findings verified against current source (file:line) — not the prior `reviews/01-architecture.md`.

---

## Executive summary

myharness is a **sound-with-caveats** architecture. The core design choice — a channel-decoupled multi-agent runtime where the brain knows nothing about the UI — is genuinely realized: `src/lib.rs` exposes only `agent/app/config/error/memory/project/provider/runtime/safety_rules/tool/workflow`, none of which import Tauri or serde-IPC concerns, and the IPC bridge correctly lives in the binary crate (`src-tauri/`). The single most important correctness property — **per-agent workflow independence** — is now real: `AgentLoopFactory` builds each agent its own `Workflow` + `ToolRegistry` (`src/agent/factory.rs:198-248`), proven by `two_builds_produce_independent_workflows` (`src/agent/factory.rs:412`). This was a prior Critical finding and it is fixed.

The remaining architectural risks are **scaling cliffs, not correctness bugs**:

1. **Memory is a single shared `Mutex<Connection>`** (`src/memory/mod.rs:103`) serializing every recall/write/access across all agents — multi-agent concurrency will bottleneck hard on SQLite.
2. **`AgentLoop` is a 2376-line god-object** (`src/agent/mod.rs`) mixing prompt-building, streaming, tool dispatch, approval, memory recall, cache heuristics, and constitution reloading. It is the single largest evolvability risk.
3. **`cleanup_for_agent` drops ALL pending approvals on any agent exit** (`src-tauri/src/ipc/approval.rs:65-77`) — correct today only because the design guarantees at most one pending approval per agent, but the data structure doesn't encode that invariant and multi-agent concurrent approvals would mis-resolve.
4. **Provider/model swap is "half-runtime"** — the factory swaps for *newly built* agents (`factory.rs:154-162`) but live agents must be swapped separately per-loop; the two paths can drift.

None of these block the current single-primary-agent product. They define where the "multi-agent-ready from day one" claim (PLAN.md line 6) stops being true.

---

## Overall architecture verdict: **Sound-with-caveats**

The dependency direction is clean, the channel contract is faithful to the PRD, the fan-in/forwarder/dead-agent-cleanup logic is carefully reasoned and tested, and the plan-first workflow's disk-as-source-of-truth design is correctly implemented. The caveats are concurrency-throughput and god-object-size, addressed below.

---

## 1. Module boundaries & dependency direction — ✅ Sound

The brain has no UI/IPC dependency. `src/lib.rs:7-17` exports 11 modules; none pull in `tauri`, `serde_json` for IPC payloads, or any `ipc` module. The IPC bridge sits in the binary crate (`src-tauri/src/ipc/`), and the lib's only serialization concession is `Serialize`/`Deserialize` on the *channel* types (`SerializableAgentEvent`, `ToolResult`, `Approval`, `WorkflowState`) in `src/runtime/channels.rs:154-196` and `src/provider/*` — which is legitimate shared vocabulary, not a leak. The `AgentSpawner` trait (`src/runtime/mod.rs:180-198`) is the deliberate seam: the brain's `spawn_agent` tool depends only on a trait, and the IPC layer injects the concrete `IpcSpawner` (`src-tauri/src/ipc/commands.rs:330`). This is textbook dependency inversion and it works.

**Observation (Low):** `src/provider/_dbg_test.rs` (16 lines) looks like a leftover debug shim inside the published module tree. Not architecture, but it's noise in the module boundary surface. Evidence: `src/provider/_dbg_test.rs` (baseline inventory).

---

## 2. Multi-agent runtime — ✅ Sound (with a throughput caveat)

The fan-in design is correct and well-reasoned. `AgentManager` (`src/runtime/mod.rs:19-167`) owns agent handles + a bounded `mpsc` fan-in channel (capacity 256, `src-tauri/src/main.rs:68`). Each agent task clones the fan-in sender; events from all agents interleave safely because each `AgentEvent` is consumed by the single forwarder task (`src-tauri/src/ipc/events.rs:61-203`) which serializes-and-emits one at a time — no interleaving hazard at the emit boundary. The forwarder owns the receiver directly (no manager lock held while awaiting `recv()`), which is explicitly called out to avoid deadlocking Tauri commands (`events.rs:33-41`, `mod.rs:72-80`). Backpressure exists via the bounded fan-in channel: if the frontend stops emitting, agent `send().await` blocks — acceptable, and self-limiting.

`mgr.send` uses `try_send` (non-blocking, `mod.rs:57`) which returns the command on full/closed rather than blocking — a deliberate choice so the cleanup path (`events.rs:296-313`) never awaits while holding the manager lock. Good.

**Caveat — Medium (throughput):** The fan-in is a **single global serial point**. With N agents each streaming TextDeltas at 50-200 tok/s, all events funnel through one forwarder → one `app.emit` per event. The forwarder also locks the manager on every `Started`/`Finished`/`Exited`/approval (`events.rs:135,142,162,184`). At low agent counts this is fine; at 3+ simultaneous streaming agents the per-event manager lock acquisitions will serialize and the single emit thread becomes the throughput ceiling. Evidence: `src-tauri/src/ipc/events.rs:133-191`.

---

## 3. Per-agent Workflow independence — ✅ Sound (verified)

The prior Critical (shared singleton workflow) is **fixed**. `AgentLoopFactory::build_inner` creates a *fresh* `Arc<Mutex<Workflow>>` per build (`factory.rs:201-205`) and a *fresh* `ToolRegistry` wired to it (`factory.rs:209,253-300`). The workflow tools (`create_plan`/`complete_step`/`update_plan`/`abandon_plan`) hold clones of that per-agent workflow Arc, so a plan created by agent A cannot affect agent B. Proven by `two_builds_produce_independent_workflows` (`factory.rs:412-457`) and the integration test `per_agent_workflows_are_independent_via_factory` (`tests/workflow_integration.rs:369`).

The **shared** deps are correctly chosen: provider (stateless), sandbox (immutable post-construction), safety_rules (mtime-checked read-only from the loop's view), safety_mode (intentionally global — a runtime toggle is a global setting, `factory.rs:24-25`), plans_dir (shared path but each workflow reads its own plan file), constitution_source (Clone, mtime-checked per turn). **Memory is the one shared dep that is concurrency-correct but a throughput bottleneck** — see §5.

**Observation (Low):** `build_inner` loads `wf.load_latest()` and **ignores** the result (`factory.rs:203`, `let _ =`). If the plans dir is empty/corrupt the agent silently starts in Planning with no plan — that's the intended behavior, but the `let _` swallows a real I/O error that a noisy-log would help diagnose. Evidence: `src/agent/factory.rs:203`.

---

## 4. IPC bridge — ✅ Sound (with one correctness-fragility point)

The oneshot-approval design is exactly as the PRD specifies (gotcha #5). `AgentEvent::ApprovalRequest` carries a non-serializable `oneshot::Sender` (`channels.rs:66-72`); `into_serializable` extracts the sender and returns it separately (`channels.rs:219-233`); the forwarder stores it in `PendingApprovals` keyed by `tool_call_id` *before* emitting the serializable event (`events.rs:177-191`); the `approve` command resolves it (`approval.rs:45-53`). Correct.

Dead-agent cleanup is correct: `Finished` does **not** remove the agent (it stays alive for follow-up prompts — `events.rs:138-157`, with a regression test `agent_task_stays_alive_after_turn` at `src/runtime/agent.rs:531`); only `Exited` (task truly terminated) removes it from manager + loop map and cleans approvals (`events.rs:158-172`). The `Complete → Executing` transition cleans up **inactive** subagents while leaving running ones alone (`events.rs:76-84,296-313`), with careful reasoning about FIFO ordering under a single manager lock. This is genuinely well-engineered.

**Correctness-fragility — Medium:** `PendingApprovals::cleanup_for_agent` drops **ALL** pending approvals regardless of which agent exited (`approval.rs:65-77`). The comment admits the map is keyed by `tool_call_id`, not `(agent_id, tool_call_id)`, so it can't target one agent's approvals. This is **safe today** only because of an unstated invariant: at most one agent is blocked on an approval at a time (the approval gate is per-turn, and the product is effectively single-primary-agent). The data structure does **not** encode that invariant. If two agents ever have concurrent pending approvals and one exits, the other's approval sender is dropped → the still-alive agent's `oneshot::Receiver` gets `Err` → its tool call fails with a spurious "approval dropped" error. The fix (key by `(agent_id, tool_call_id)`) is acknowledged in the comment but not done. Evidence: `src-tauri/src/ipc/approval.rs:65-77`.

---

## 5. State ownership & concurrency — ⚠️ Sound-but-bottlenecked

Enumerated lock inventory (verified by search):

| Guard | Location | Protects | Notes |
|---|---|---|---|
| `tokio::Mutex<AgentManager>` | `state.rs:33` | agent registry | Held briefly by commands + forwarder; forwarder never holds across `recv()`. ✅ |
| `tokio::Mutex<HashMap<AgentId, Arc<AgentLoop>>>` | `state.rs:38` | per-agent loops | Insert on spawn, remove on Exited. ✅ |
| `tokio::Mutex<Project>` | `state.rs:43` | project paths | Read by read_file/list_files/git. ✅ |
| `tokio::Mutex<Config>` | `state.rs:45` | global config | Held briefly (`commands.rs:423` notes "short — no awaits"). ✅ |
| `std::sync::Mutex<HashMap>` | `approval.rs:18` | pending oneshots | Brief. ✅ |
| `tokio::Mutex<BacklogStore>` | `state.rs:66` | backlog.json | ✅ |
| `tokio::Mutex<Option<RunAllState>>` | `state.rs:74` | run-all loop | ✅ |
| `RwLock<Arc<dyn LlmClient>>` | `factory.rs:69`, `mod.rs:46` | swappable provider | Read per turn/build, write on model swap. ✅ |
| `RwLock<ContextManager>` | `factory.rs:72` | context template | Swapped with provider. ✅ |
| `RwLock<SafetyMode>` | `factory.rs:81`, `mod.rs:64` | global safety mode | Intentionally shared global. ✅ |
| `RwLock<Option<Arc<dyn AgentSpawner>>>` | `factory.rs:92` | spawner | Set once at startup. ✅ |
| `tokio::Mutex<Workflow>` | per agent | plan state | One per agent — independent. ✅ |
| **`tokio::Mutex<Connection>`** | `memory/mod.rs:103` | **the single SQLite conn** | **Shared across ALL agents. Serializes every memory op.** ⚠️ |
| `std::sync::Mutex<ConstitutionSource>` | `mod.rs:101` | constitution cache | Brief; not held across await (returns owned clone, `mod.rs:110-120`). ✅ |
| `std::sync::Mutex<Option<String>>` / `Option<u32>` / `Option<AgentId>` | `mod.rs:82,87,93` | session id / cache / agent id | Brief std mutexes inside AgentLoop. ✅ |

**Lock ordering / deadlock:** I found **no lock-ordering hazard and no await-while-holding-lock** in the production paths. The forwarder explicitly drops the manager guard before any further await (`events.rs:146,165,248`). `cleanup_inactive_subagents` holds the manager lock across the `try_send` loop but `try_send` is non-blocking (`events.rs:296-313`, comment at `:288-295`). The startup `block_on` locks in `main.rs:95-109` are uncontended. This is disciplined.

**The real concurrency finding — High (scaling cliff):** `MemoryStore` wraps a **single** `Connection` in one `tokio::Mutex` (`memory/mod.rs:103,114`), and every memory operation — recall, write, access-count bump, consolidation, session start/end — does `self.conn.lock().await` (`memory/mod.rs:218,352,384,437,454,481,499,508,545,568,621,688`). Auto-recall runs on every turn and the recall path is an O(N) scan over all memories (per baseline + prior perf review). With multiple agents each running turns, **all memory access serializes on one lock** and recall scans run under it. This is concurrency-*safe* (no data race) but a hard throughput cliff for multi-agent. SQLite supports WAL + multi-connection read concurrency; the single-connection design forgoes that. Evidence: `src/memory/mod.rs:103,218,…,688`.

**Std-mutex-inside-async caveat — Low:** `AgentLoop`'s `session_id`/`last_prompt_tokens`/`agent_id` use `std::sync::Mutex` and are locked in async contexts (`mod.rs:238,244,250,257`). These are never held across an `.await` (they clone/assign and release), so no deadlock — but if a future change holds one across await it would block the async runtime. A `tokio::sync::Mutex` or parking_lot would be safer-by-construction. Evidence: `src/agent/mod.rs:82-93`.

---

## 6. Error-propagation topology — ✅ Sound

Errors flow correctly and observably: tool errors → fed back as `tool`-role messages (`mod.rs` loop, MAX_RETRIES=3 at `:38`); provider errors �� retried with exponential backoff (1s,2s,4s) in `AgentTask::run_turn_with_retry` (`src/runtime/agent.rs:43-97`), surfaced as `AgentEvent::Error { retrying }` so the frontend distinguishes transient vs final without string-matching (`channels.rs:138-147`, `agent.rs:68-91`). Final errors on the main agent propagate to the backlog Run-All loop for rollback (`events.rs:104-126`). Emit failures are `eprintln!`'d, not fatal (`events.rs:199,270`). Memory/safety-rules/store failures degrade silently to in-memory/empty fallbacks at startup (`main.rs:312-340`) — acceptable, with the startup-error screen as the catch-all (`App.tsx:167-191`).

**Observation (Low):** Provider retry lives in `AgentTask` (`runtime/agent.rs`), but `MAX_RETRIES` for *tool* errors lives in `AgentLoop` (`agent/mod.rs:38`). Two separate retry concepts with the same constant name in two modules — easy to confuse. Not a bug; a clarity issue. Evidence: `src/runtime/agent.rs:48` vs `src/agent/mod.rs:38`.

---

## 7. Brain/UI decoupling claim — ✅ Largely true

The PRD's "brain fully decoupled from the UI via typed channels" holds for the *core* brain. The one wrinkle: the channel types in `src/runtime/channels.rs` carry `serde::{Serialize, Deserialize}` derives and a `SerializableAgentEvent` mirror (`channels.rs:154-196`) whose only purpose is the IPC boundary. This is a **justified, minimal** serialization concession — the brain needs *some* serializable event form to be usable by any UI host, not just Tauri. It does not make the brain depend on Tauri. The `AgentSpawner` trait (§1) keeps the spawn capability inverted. Verdict: decoupling is real; the serde derives are the cost of a channel contract and are acceptable.

**Minor (Low):** `AgentEvent` itself is NOT `Serialize` (only `SerializableAgentEvent` is), and `into_serializable` is a hand-written 1:1 match (`channels.rs:202-298`). Keeping two parallel enums in sync is a maintenance tax — adding an `AgentEvent` variant requires editing the match in 3 places (the enum, the serializable enum, the match). A `#[serde(skip)]` on the oneshot field would be less safe (the oneshot must never serialize) but the current approach is the conservative correct one. Evidence: `src/runtime/channels.rs:46-196,202-298`.

---

## 8. Extensibility — ⚠️ Mixed

- **New agent tool:** Moderate. Implement the `Tool` trait, register in `AgentLoopFactory::build_registry` (`factory.rs:262-299`). The registry is a flat list; no category metadata beyond what `ToolFilter` keys on. Adding a tool that should be gated by workflow requires also touching `Workflow::allowed_tools`/`ToolFilter`. ~3 files. Acceptable.
- **New provider kind:** Easy in principle (implement `LlmClient`, `ProviderKind` is a 2-variant enum `provider/mod.rs`), but the capability-aware logic is sprinkled through the 1554-line `provider/openai.rs` and the agent loop's capability checks. A genuinely new provider shape (not OpenAI-compatible) would require broad changes. The "one client, swap base URL" decision (`PLAN.md` line 61) intentionally limits this — fair.
- **New right-panel view:** Easy — add a component under `frontend/src/components/views/`, register in `RightPanel.tsx`. Clean seam. ✅
- **New workflow state:** Hard. `WorkflowState` is a 3-variant enum (`Planning/Executing/Complete`) threaded through `allowed_tools`, the system-prompt builder (`prompt.rs:87`), the ToolFilter, plan-file parsing, and the forwarder's `prev_workflow_state` transition logic (`events.rs:58-84`). Adding a state is a wide change. Not currently needed, but it's the least extensible axis.

---

## 9. Multi-agent-readiness claim — ⚠️ Partially overstated

PLAN.md line 6/11 claims "multi-agent-ready from day one." What's actually true:

- ✅ **Spawning** multiple agents works (factory + manager + channels).
- ✅ **Per-agent workflow/plan state** is independent (§3, tested).
- ✅ **Event fan-in + completion notification** (parent/child) works and is tested (`runtime/agent.rs:531`, `channels.rs` tests).
- ⚠️ **Memory throughput** collapses under concurrent agents (§5 — single `Mutex<Connection>`).
- ⚠️ **Pending approvals** can mis-resolve under concurrent approvals (§4 — `cleanup_for_agent` clears all).
- ⚠️ **Provider/safety-mode/config are global**, which is *intended* but means a model swap or safety toggle affects all agents mid-turn — defensible, but "multi-agent-ready" should note these are shared-global, not per-agent.

Verdict: the claim is **true for spawning and plan isolation**, **overstated for concurrent throughput and approval isolation**. With 2-3 agents it works; the bottlenecks are the memory lock and the approval map. Recommend PLAN.md qualify the claim.

---

## 10. Largest-file / god-object risk — High

`src/agent/mod.rs` is **2376 lines** and `AgentLoop` (struct at `:41-94`) holds 13 fields spanning: provider, context manager, tools, workflow, sandbox, constitution, safety_mode, memory, safety_rules, vision, session_id, last_prompt_tokens, agent_id. Its `impl` block handles system-prompt assembly, streaming accumulation, tool-call dispatch, the approval gate, auto-recall injection, working-memory capture, token-cache heuristics, and constitution reloading. This is a god-object.

`src/runtime/agent.rs` (1030) is large but cohesive (the `AgentTask` turn driver + image handling). `src/provider/openai.rs` (1554) is large because it owns the entire OpenAI wire protocol + stream parsing — splitting is possible (stream parsing vs. request building) but lower payoff. `src/memory/mod.rs` (1211) and `src-tauri/src/ipc/commands.rs` (1191) are large; `commands.rs` is a flat bag of `#[tauri::command]` fns which is the Tauri-idiomatic shape.

**The `AgentLoop` split is the highest-value refactor.** Natural seams: (a) a `PromptBuilder` (constitution + workflow + recall → system prompt), (b) a `ToolDispatcher` (approval + exec + result feedback), (c) a `StreamAccumulator` (already partly exists as `DeltaAccumulator`), (d) moving session/cache heuristics out. This would take `mod.rs` from 2376 → ~3-4 files of 400-700 lines and make each responsibility independently testable. Evidence: `src/agent/mod.rs:41-94`, 2376 total lines.

---

## What's done well

- **Per-agent workflow isolation** — the prior Critical is fixed and tested (`factory.rs:412`).
- **The oneshot-across-IPC boundary** (gotcha #5) is handled exactly right with a clean split + map.
- **`Finished` vs `Exited` distinction** — carefully reasoned and regression-tested (`events.rs:8-14`, `runtime/agent.rs:531`). Many harnesses get this wrong (killing the agent after every turn).
- **Lock discipline** — no await-while-locking found in production paths; the forwarder owns the receiver to avoid deadlocking commands; `try_send` used deliberately in lock-holding paths.
- **Dependency inversion via `AgentSpawner`** — the brain spawns agents without knowing the IPC layer.
- **Disk-as-source-of-truth plans** with `load_latest` resume (`factory.rs:199-205`, tested `factory.rs:459`).

---

## Top 3 architectural risks

1. **`AgentLoop` god-object (2376 lines, 13 responsibilities)** — `src/agent/mod.rs:41-94`. Highest evolvability cost; split along prompt-build / tool-dispatch / stream-accumulate seams. **High.**
2. **Single shared `Mutex<Connection>` memory store** — `src/memory/mod.rs:103`. Serializes all agent memory access; the multi-agent throughput cliff. Consider a connection pool / read-write split (WAL) or per-agent read connections. **High.**
3. **`PendingApprovals` keyed by `tool_call_id` only; `cleanup_for_agent` clears all** — `src-tauri/src/ipc/approval.rs:65-77`. Safe today by an unenforced invariant; will mis-resolve under concurrent multi-agent approvals. Key by `(agent_id, tool_call_id)`. **Medium.**