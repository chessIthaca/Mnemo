## Verdict: FINDINGS (0 high, 5 low)

*Breakdown: 0 CRITICAL, 0 HIGH, 2 MEDIUM (M1, M2), 3 LOW (L1, L2, L3). The verdict line counts non-high findings; severities are tagged per-finding below.*

# Architecture & Design Review — Mnemo (2026-10-15, HEAD)

**Perspective:** Architecture & design — module boundaries, layering, coupling/cohesion, state machine, multi-agent isolation, error handling, extensibility, config scoping, abstraction quality.
**Reviewer:** read-only architecture reviewer (spawned sub-agent).
**Grounding:** current source (`src/`, `src-tauri/src/`, `frontend/`), prior reviews (2026-04-08 perf, 2026-08-13 quality/arch, 2026-04-18 deep arch, 2026-09-15 arch), `PLAN.md`, `Cargo.toml`. Every finding cites file:line.

The architecture is **sound and has measurably improved** since the 2026-04-18 deep review: five of that review's nine findings (A1, A2, A4, A6, A7) are resolved, and two prior 2026-08-13 findings (`never_auto` dead weight, `ToolFilter` Skill→Planning default) are resolved. The three-layer split (brain `src/` → Tauri shell `src-tauri/` → React frontend) is genuinely clean at the dependency level — the brain has **zero** `tauri::` imports (verified: the only match in `src/**/*.rs` is a doc-comment mention at `src/config/mod.rs:476`). The two carry-forward findings (A3, A5) are the same domain-logic-in-the-adapter concern, and both have *grown* rather than been addressed. No critical or high-severity defects.

---

## 1. Prior-findings verification (mandate: verify, don't repeat)

| Prior finding | Source | Status | Evidence |
|---|---|---|---|
| A1 — blocking git in forwarder | 2026-04-18 HIGH | **RESOLVED** | `src/project/git_ops.rs` runs all git ops via `tokio::task::spawn_blocking` (doc :9-10); `run_all.rs:515` "now spawn_blocking", root cloned before checkpoint (`:519`, `:708`) |
| A2 — shared plans_dir, parentless agents clobber stack.json | 2026-04-18 HIGH | **RESOLVED** | `factory.rs:547` `build_with_id_and_plans_dir` + per-agent `agents/<id>/` subdir; regression test `two_per_agent_dirs_dont_clobber` (`factory.rs:1965`); `reviews_dir` always uses main dir (`factory.rs:805`) |
| A3 — event forwarder is orchestration brain | 2026-04-18 MEDIUM | **NOT ADDRESSED — GREW** | `events.rs` 747→**1757** lines; still owns `TurnResolveLatch` (:110), `notify_parent_on_completion` (:829), failed-reviewer protocol (:867) |
| A4 — BacklogStore + run-all git ops in adapter | 2026-04-18 MEDIUM | **RESOLVED** | `BacklogStore` moved to lib (`src/backlog.rs:96`); git ops in `src/project/git_ops.rs` with `GitRunner`/`RealGit`/`MockGit` trait (:53-75) |
| A5 — settings.rs god-module | 2026-04-18 MEDIUM / 2026-08-13 HIGH | **NOT ADDRESSED — GREW** | `settings.rs` 1746→**1917** lines; still owns config assembly + endpoint/key/pricing persistence + domain validation + live model listing + runtime rewires |
| A6 — duplicated re-embed fingerprint logic | 2026-04-18 LOW/MED | **RESOLVED** | both `main.rs:1386` and `rewire.rs:65` call the single `mnemo::memory::reembed_if_needed` |
| A7 — startup pull pattern | 2026-04-18 LOW | **RESOLVED** | `startup_snapshot` added (`startup.rs:50`, registered `main.rs:689`) |
| `never_auto()` dead weight | 2026-08-13 MEDIUM | **RESOLVED** | no matches for `fn never_auto(&self)`; superseded by `never_auto_for` (`tool/mod.rs:132`) |
| `ToolFilter::from_state` Skill→Planning defensive default | 2026-08-13 MEDIUM | **RESOLVED** | now returns `Option<Self>`, `None` for `Skill` (`tool/mod.rs:250-258`) |
| Error variants are string buckets | 2026-08-13 HIGH | **NOT ADDRESSED** (accepted trade-off) | `error.rs` still `Provider(String)`/`Memory(String)`/`Tool(String)`; `is_rate_limited`/`is_non_retryable` do substring matching |
| IPC commands flatten errors to String | 2026-08-13 MEDIUM | **IMPROVED** | `settings.rs` now returns `Result<_, IpcError>` (e.g. `:28`, `:982`) |

---

## 2. Module boundaries & layering (mandate 1) — CLEAN ✓

The lib.rs doc claim that the brain is "fully usable without the TUI" (`src/lib.rs:8-10`) **holds at the dependency level**:

- **Zero `tauri::` imports in `src/`** — verified by exhaustive search; the single hit (`config/mod.rs:476`) is a doc comment referencing `tauri::process::restart`, not a code dependency.
- **Spawn dependency is inverted via traits**, not a hard dependency: `AgentSpawner` / `ParentAwareSpawner` / `DescendantTracker` (`runtime/mod.rs:194-275`) are brain-defined traits the IPC layer implements and injects. The `spawn_agent` tool depends on the trait, never on `AppHandle`.
- **`app/` module is now a stub** — `src/app/mod.rs` (20 lines) is just a panic-hook installer; the old `AppState`/`TranscriptEntry`/`handle_agent_event` types were removed when the Tauri swap happened (doc :7-9). No leak.
- **Channel types carry serde derives as a documented concession** (`SerializableAgentEvent` etc.) — these are brain-side (`runtime/channels`) so the IPC layer can serialize them without re-mapping. Prior 2026-09-15 review (A4) accepted this; it remains a reasonable trade-off.

**The one residual leak is A3/A5** (below): domain orchestration and config-assembly logic live in `src-tauri/src/ipc/`, not `src/`. The brain is usable without the TUI for the *agent loop*, but the *run-all backlog dispatch*, *turn-resolution exclusivity*, *parent-notification*, and *settings assembly* would have to be reimplemented by a non-Tauri host.

---

## 3. Coupling & cohesion (mandate 2)

**Largest files (line counts at HEAD):**

| File | Lines | Cohesion verdict |
|---|---|---|
| `src/memory/mod.rs` | 4046 | Large but cohesive — trait + impl + tests in one module. Splitting impl/tests out would help navigation. |
| `src/tool/workflow/plan.rs` | 3353 | Cohesive — all workflow-plan tools (`create_plan`/`update_plan`/`complete_step`/`abandon_plan`/`finish`/skills). Could split per-tool-group. |
| `src/workflow/mod.rs` | 2233 | Cohesive — the state machine. |
| `src/tool/mod.rs` | 2047 | Trait + registry + filter + dispatch glue. Splittable. |
| `src/agent/turn.rs` | 2075 | The turn driver — large single `impl` block (deliberate, reads private state). |
| `src/agent/factory.rs` | 1993 | Cohesive — per-agent construction. |
| `src-tauri/src/ipc/settings.rs` | 1917 | **God-module — see M2.** |
| `src-tauri/src/ipc/events.rs` | 1757 | **Orchestration-in-adapter — see M1.** |

**Blast-radius spot check:** `graph_impact` on `Workflow` (`workflow/mod.rs:122`) returns a narrow fan-out — the struct is consumed almost exclusively through `Arc<Mutex<Workflow>>` held by tool instances, so changes to its fields are well-contained. The `AgentLoop` struct (`loop_impl.rs:91-248`) has ~25 fields (see L3) but is similarly accessed through a single `Arc`.

---

## 4. The plan-first state machine (mandate 3) — SOUND ✓

- **Transitions are guarded and well-defined.** Each mutating method (`complete_step`, `finish`, `start_skill`, `end_skill`, `abandon_plan`, `create_plan`) checks the current state and returns `WorkflowWrongState` on illegal transitions (`workflow/mod.rs` throughout). `complete_step` correctly differentiates root-plan-finish (→ Reviewing for Implementation/BugFixing, → Complete for Research) from sub-plan-finish (pop → Executing).
- **The review-gate is robust.** `finish` (`plan.rs:1238-1275`) canonicalizes the report path and verifies it `starts_with` the factory's `reviews_dir` (path-traversal guard, :1242-1246), checks the file is non-empty, then `parse_verdict` (:1061) requires the first non-empty line to be `## Verdict: PASS` or `## Verdict: FINDINGS (n high, n low)`; **unparseable → error (fail-closed)**. Bug-fixing plans additionally require a `regression_test` name and a `BUG:` memory write. The `reviewed` flag is persisted and restored by `load_latest` *before* the skill early-return (:946), so a restart cannot corrupt the gate.
- **No inconsistent reachable state found.** `ToolFilter::from_state` returns `Option` and `None` for `Skill` (the prior defensive-default bug is fixed). `enter_planning_for_task` (Complete→Planning) is deliberately not persisted — a documented trade-off, not a defect.

---

## 5. Multi-agent runtime isolation (mandate 4) — SOUND, with one accepted trade-off

- **Per-agent isolation is real for plan state and tools:** `factory.rs:199` `build_inner` creates a **fresh `Workflow` + fresh `ToolRegistry`** per agent. Per-agent `plans_dir` override (`build_with_id_and_plans_dir`, :547) prevents parentless UI agents from clobbering each other's `stack.json`, with a regression test (`two_per_agent_dirs_dont_clobber`, :1965).
- **Shared, by design:** `memory`, `sandbox`, `safety_mode`, `provider`, `vision`, `codegraph`, `mcp` are `Arc`-cloned from the factory into every agent (`factory.rs:606-616`). Memory is project-scoped (intentional), protected by the WAL read/write-conn split (`memory/mod.rs:388-394`: `write_conn` is the single serialized writer, `read_conn` serves reads, readers never block). The sandbox is stateless-per-call. This is an **accepted trade-off**: a child agent with `memory_write` shares the parent's memory DB — but the reviewer subagent is tool-allowlist-constrained (cannot call `memory_write`), and project-wide memory is the intended semantics.
- **The failed-reviewer protocol is exemplary** (`events.rs:867-884`): a `role:"reviewer"` child that fails with no report sets the parent's `reviewer_failure_pending` latch, which denies further reviewer spawns at dispatch until the parent asks the user — preventing the blind-respawn thrash loop. Distinct failure text directs the parent to `ask_user` (retry on another model / abandon). This is robust, well-reasoned isolation of a failure mode.
- **No shared mutable state breaks isolation:** `AgentManager` is a single `Arc<Mutex<HashMap>>` (prior 2026-09-15 A1 accepted this as the serialization point for single-user scale). Parent/child tracking is read under short lock scopes; `notify_parent_on_completion` (:829) clones the needed data out of the lock before any `send`/await (:837-853, :861-865).

---

## 6. Error-handling & recovery (mandate 5) — SOUND ✓

- **Consistent typed error:** `error.rs` is a single `thiserror` enum (`MnemoError`) with `is_non_retryable()` / `is_rate_limited()` classification; the binary boundary converts to `anyhow`. Good test coverage of the classification.
- **Retry caps are sound:** `complete_with_retry` (`dispatch.rs:635`) does 3 attempts with exponential backoff (1s→2s, :643); non-retryable and rate-limited errors fail immediately (no wasted retry). `MAX_BAD_JSON_RETRIES=8` (`turn.rs:25`) handles malformed/truncated tool-call JSON with a sanitization re-parse attempt (`turn.rs:1311-1450`) and a counter that resets on success — feeding the error back as a tool message so the model retries rather than the loop dying. This is robust error recovery.
- **See L1** for the string-bucket brittleness (accepted trade-off).

---

## 7. Extensibility (mandate 6) — WELL-DESIGNED ✓

- **New tool:** implement the `Tool` trait (`tool/mod.rs`) — `category`/`safety`/`deferred_group`/`available`/`never_auto_for` form a clean, declarative surface; the registry auto-discovers and the `ToolFilter` gates by state. Low friction.
- **New provider:** implement `LlmClient` (`provider/mod.rs:376`) — async streaming, capability-based (`Capabilities`), with default no-op trace methods for test mocks. Adding OpenAI/Anthropic/local is a trait impl + factory wiring. Clean.
- **New MCP server:** declared in `endpoints.toml`/config; `mcp/mod.rs` speaks exactly three JSON-RPC methods (`initialize`/`tools/list`/`tools/call`) over stdio+remote transports, lazy-connected, secrets via env-var names (never logged). Drop-in, no recompile for config-only servers.
- **New skill:** a TOML file in the skills dir (`skill/mod.rs`) — `{tool allow-list, prompt, entry/exit states}`; `SkillRegistry` best-effort loads at startup. Drop-a-file, no recompile. Clean.

---

## 8. Config scoping (mandate 7) — CLEAN ✓

- **Global config** lives in `~/.mnemo/` split across five files (`config/mod.rs:7-13`): `config.toml` (prefs), `endpoints.toml` (endpoints), `keys.toml` (secrets), plus pricing/embedder. **Keys are isolated** and never logged. Project config (`.coding/`, `agent.md`, `endpoints.toml` overrides) is separate.
- **No config reloads on hot paths** — verified: no `load_config`/`read_config`/`Config::load` calls in `src/agent/**/*.rs`. The provider is read once per turn into a local snapshot (`loop_impl.rs:92-96`); runtime model swaps take effect next turn. The constitution is mtime-checked (`ConstitutionHolder`, `loop_impl.rs:251`) rather than re-read every turn.

---

## 9. Abstraction quality (mandate 8) — RIGHT LEVEL, with notes

- **`AgentLoop`** — the central abstraction; ~25 fields (`loop_impl.rs:91-248`) is high, but each is individually justified with a detailed doc comment, and the provider-resolution chain (`forced_model`/`explicit_provider`/`pending_swap`/`resolved_model`/`resolved_provider`) encodes real, documented product requirements (per-agent picker, skill overrides, deferred context-shrink swaps). Prior 2026-09-15 (A2) accepted this as "justified." See L3.
- **`AgentManager`** — appropriately thin (spawn/route/fan-in/parent-tracking). Not over-engineered.
- **`Workflow`** — the right level; the state machine + plan-stack + skill-overlay + tool-allowlist is cohesive.
- **`MemoryStore`** — well-abstracted behind `MemoryStoreTrait` (`memory/mod.rs:350`), with the WAL split as an implementation detail. The trait/impl/test colocation in one 4046-line file is the main cohesion smell (see L2).
- **`Sandbox`** — clean, stateless-per-call.
- **`ToolFilter`** — clean; `from_state` returning `Option` is the correct shape after the Skill fix.

---

## Findings

### M1 — [MEDIUM] Event forwarder carries domain orchestration that belongs in the brain (A3, grew)
**Evidence:** `src-tauri/src/ipc/events.rs` (1757 lines, up from 747). It owns:
- `TurnResolveLatch` (:110) — run-all terminal-exclusivity policy (a turn must resolve the backlog at most once; first terminal outcome wins).
- `notify_parent_on_completion` (:829) — parent/child completion routing + the failed-reviewer protocol (:867-884) + review-report-path capture (:861-865).
- `cleanup_inactive_subagents` and workflow-state transition tracking (`workflow_transition_counts` :253).

**Why it matters:** This is domain policy, not UI bridging. The lib.rs claim "fully usable without the TUI" is violated for run-all dispatch, turn-resolution exclusivity, and parent-notification — a non-Tauri host must reimplement them. The 2026-04-18 review recommended extracting an `EventCoordinator` into the lib; that has not happened and the file has more than doubled.
**Recommendation:** Move `TurnResolveLatch` and `notify_parent_on_completion` (incl. the failed-reviewer protocol) into `src/runtime/` behind a brain-side trait the forwarder calls; keep only the Tauri-emit + oneshot-storage in `events.rs`.

### M2 — [MEDIUM] `settings.rs` is a god-module mixing config assembly + domain validation in the adapter (A5, grew)
**Evidence:** `src-tauri/src/ipc/settings.rs` (1917 lines, up from 1746). Its own header (`:7-9`) admits it "Owns the full Settings surface": `get_config`/`get_settings` payloads, endpoint + API-key + pricing persistence (`save_endpoints` :162), the non-endpoint settings patch (`save_settings` :982) with inline domain validation, live model listing, and runtime provider rewires.
**Why it matters:** Domain validation and config assembly belong in `src/config/`, not the IPC adapter. This is the same layering concern as M1 and has been flagged across three reviews (2026-08-13 High, 2026-04-18 A5) while growing.
**Recommendation:** Extract a `SettingsAssembler`/validator into `src/config/`; leave `settings.rs` as a thin Tauri-command shim that delegates.

### L1 — [LOW] Error type uses string buckets, forcing substring-matching for retry classification (accepted trade-off)
**Evidence:** `error.rs` variants are `Provider(String)`/`Memory(String)`/`Tool(String)`; `is_rate_limited`/`is_non_retryable` do `contains(...)` substring matches. Flagged 2026-08-13 (High); left as a documented `thiserror`+String simplification.
**Why it matters:** Substring matching is brittle (a provider error message containing "rate" but not rate-limited would mis-classify). Low impact today because the matching strings are narrow, but it is the kind of thing that silently breaks on a provider wording change.
**Recommendation:** Acceptable for now; if a mis-classification bug appears, promote to structured variants.

### L2 — [LOW] Several core lib files exceed 2000 lines (cohesion/navigation, not correctness)
**Evidence:** `memory/mod.rs` (4046), `plan.rs` (3353), `workflow/mod.rs` (2233), `tool/mod.rs` (2047), `turn.rs` (2075), `factory.rs` (1993).
**Why it matters:** Navigation cost, not a defect. `memory/mod.rs` in particular collocates trait + impl + 1000+ lines of tests.
**Recommendation:** Split `memory/mod.rs` into `store.rs` (impl) + `tests.rs`; split `plan.rs` by tool group. Optional.

### L3 — [LOW] `AgentLoop` field count is high (~25) (accepted trade-off)
**Evidence:** `loop_impl.rs:91-248`. The provider-resolution chain alone is 5 fields (`forced_model`, `explicit_provider`, `pending_swap`, `resolved_model`, `resolved_provider`).
**Why it matters:** Each field is justified by a documented product requirement; prior 2026-09-15 (A2) accepted this. Noting only because the count is at the edge where a future refactor (e.g. a `ProviderResolution` sub-struct) would aid clarity.
**Recommendation:** Acceptable; consider grouping the 5 provider-resolution fields into a sub-struct if more are added.

---

## Strengths worth preserving

1. **Genuinely clean three-layer split** — zero `tauri::` deps in the brain; spawn/runtime seams are trait-inverted (`AgentSpawner`/`DescendantTracker`).
2. **`git_ops.rs` extraction is exemplary** — `GitRunner`/`RealGit`/`MockGit` trait + `spawn_blocking` + impl/test split (`src/project/git_ops.rs:53-182`): testable, non-blocking, in the lib.
3. **The finish review-gate** — path canonicalization + verdict parsing + fail-closed + bug regression-test requirement (`plan.rs:1238-1275`).
4. **The failed-reviewer protocol** — latch + deny-blind-respawn + ask-user guidance (`events.rs:867-884`).
5. **Per-agent plan-dir isolation with a regression test** (`factory.rs:547`, `:1965`).
6. **Extensibility seams** — `Tool`, `LlmClient`, Skill (TOML drop-a-file), MCP (lazy, 3 JSON-RPC methods) are all low-friction.
7. **Memory WAL split** — read/write conn, readers never block (`memory/mod.rs:388-394`).

---

## Summary

Architecture is sound and trending positive: 5 of 9 prior deep-review findings resolved, 2 prior quality findings resolved. The two genuine carry-forward concerns (M1 forwarder-as-orchestration, M2 settings god-module) are the *same* root issue — domain logic leaking into the IPC adapter — and both have grown. Neither blocks correctness or shipping; both are worth a dedicated extraction pass. No critical/high defects.
