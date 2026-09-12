# Review Baseline — 2026 whole-codebase review

Shared grounding for the four perspective reviews (Chief Architect, Code
Quality, UI/Usability, PRD). Every reviewer should treat this as the
authoritative current state of the codebase and read the actual files before
writing findings.

## Product

**myharness** — a coding-first agentic harness in Rust. A Tauri 2.x native
window hosts a Rust "brain" (config, project, provider, tools, memory,
workflow, runtime, agent loop) plus a React 18 + TypeScript + Vite + Tailwind
web frontend. One app instance works on exactly one project (a directory whose
`.coding/` holds config, memory, plans, and an `agent.md` constitution). The
agent runs under an enforced plan-first workflow and an approve-each-action
gate with diff-based approval for file edits.

- **PRD:** `PLAN.md` (root). Locked-in technical decisions, critical gotchas,
  module layout, and UI build phases A–E live there.
- **Project constitution (hard rules):** `agent.md` (root) + a global
  `~/.myharness/agent.md`. Both are loaded before every LLM turn.

## Build / test health (verified this session)

| Check | Result |
|---|---|
| `cargo build` | clean (exit 0) |
| `cargo test` | lib tests pass; `ipc_bridge` 9/9; `workflow_integration` 5/5; `provider_integration` 3 ignored (need a live endpoint). exit 0 |
| `frontend: tsc --noEmit` | clean (exit 0) |
| `frontend: vite build` | clean, 2077 modules, built. Note: single 596 kB JS chunk (>500 kB) triggers Vite's chunk-size warning — code-splitting not done. |

## File inventory (with line counts)

### Rust brain — `src/` (48 files)
```
src\agent\approval.rs          390
src\agent\context.rs           555
src\agent\factory.rs           528
src\agent\mod.rs              2376   <- the agent loop; largest file
src\agent\prompt.rs            206
src\app\mod.rs                  15
src\config\endpoints.rs        297
src\config\general.rs          230
src\config\keys.rs             110
src\config\mod.rs              243
src\config\projects.rs         134
src\error.rs                    42
src\lib.rs                      16
src\memory\consolidation.rs    583
src\memory\embedder.rs         195
src\memory\mod.rs             1211
src\memory\schema.rs           162
src\memory\strength.rs          75
src\memory\types.rs            355
src\project\agent_md.rs        242
src\project\mod.rs             199
src\provider\mod.rs            327
src\provider\openai.rs        1554   <- OpenAI-compatible client + stream parsing
src\provider\stream.rs         186
src\provider\vision.rs         183
src\provider\_dbg_test.rs       16   <- looks like a debug shim
src\runtime\agent.rs          1030
src\runtime\channels.rs        463
src\runtime\correction.rs      143
src\runtime\mod.rs             354
src\safety_rules.rs            595
src\tool\agent\describe_image.rs 318
src\tool\agent\file_append.rs   176
src\tool\agent\file_edit.rs     459
src\tool\agent\file_read.rs     270
src\tool\agent\file_write.rs    188
src\tool\agent\git.rs           168
src\tool\agent\mod.rs            22
src\tool\agent\sandbox.rs       215
src\tool\agent\search.rs        337
src\tool\agent\shell.rs         191
src\tool\agent\spawn_agent.rs   282
src\tool\memory\mod.rs          301
src\tool\mod.rs                 451
src\tool\workflow\mod.rs          4
src\tool\workflow\plan.rs       535
src\workflow\mod.rs             737
src\workflow\plan_file.rs       459
```

### Tauri app shell + IPC bridge — `src-tauri/` (8 files)
```
src-tauri\src\main.rs          392
src-tauri\src\ipc\approval.rs  109   <- pending-approvals map (oneshot senders)
src-tauri\src\ipc\backlog.rs   405
src-tauri\src\ipc\commands.rs 1191   <- all #[tauri::command] fns
src-tauri\src\ipc\events.rs    300   <- event forwarder + serializable payloads
src-tauri\src\ipc\mod.rs        18
src-tauri\src\ipc\run_all.rs   120
src-tauri\src\ipc\state.rs      87   <- IpcState managed state
```

### Tests — `tests/` (3 files)
```
tests\ipc_bridge.rs            207   <- serialization + approval round-trip
tests\provider_integration.rs  153   <- live endpoint tests (ignored by default)
tests\workflow_integration.rs  416   <- planning gate, full lifecycle, resume, multi-agent
```

### Frontend — `frontend/src/` (30 files)
```
frontend\src\App.tsx                          196
frontend\src\main.tsx                          12
frontend\src\components\ErrorBoundary.tsx       44
frontend\src\components\chat\ApprovalPrompt.tsx 167
frontend\src\components\chat\Conversation.tsx    72
frontend\src\components\chat\DiffView.tsx       104
frontend\src\components\chat\InflightBar.tsx    224
frontend\src\components\chat\Message.tsx        330
frontend\src\components\layout\ConfigDialog.tsx 346
frontend\src\components\layout\InputBar.tsx     350
frontend\src\components\layout\MainPanel.tsx     60
frontend\src\components\layout\RightPanel.tsx    89
frontend\src\components\layout\SafetyToggleDialog.tsx 71
frontend\src\components\layout\Sidebar.tsx       97
frontend\src\components\layout\StatusBar.tsx    540
frontend\src\components\views\BacklogView.tsx   459
frontend\src\components\views\DiffViewer.tsx    141
frontend\src\components\views\FileBrowser.tsx   157
frontend\src\components\views\MdViewer.tsx      116
frontend\src\components\views\PlanProgress.tsx  128
frontend\src\components\views\SafetyRules.tsx   131
frontend\src\components\views\StatsView.tsx     378
frontend\src\components\views\ToolOutput.tsx     82
frontend\src\hooks\useAgentEvents.ts           148
frontend\src\hooks\useAgentStore.ts           1154   <- Zustand store; largest frontend file
frontend\src\hooks\useCountUp.ts                46
frontend\src\lib\slash.ts                       44
frontend\src\lib\tauri.ts                      319
frontend\src\lib\types.ts                      157
frontend\src\styles\globals.css                178
```

## Key architectural facts (verified in source)

- **Multi-agent runtime:** `AgentManager` (runtime/) spawns agents as tokio
  tasks; UI↔agent comms are typed channels (`AgentCommand` UI→agent,
  `AgentEvent` agent→UI, fanned-in + tagged with `AgentId`). `AgentLoopFactory`
  (agent/factory.rs) holds shared stateless deps and builds each agent its own
  `Workflow` + `ToolRegistry` — so per-agent workflows are now independent
  (there is an integration test `per_agent_workflows_are_independent_via_factory`).
  `list_agents`/running-state and dead-agent cleanup: see commands.rs + runtime.
- **Provider:** one OpenAI-compatible client (`OpenAiClient`, provider/openai.rs)
  swapping base URL; `Capabilities` struct drives behavior (tool_choice, strict
  schema, reliable finish_reason, max_context). `ProviderKind::{OpenAI, Local}`.
  Tool-call deltas accumulated per-`index` in a HashMap; tool calls detected by
  presence of deltas, not solely `finish_reason`.
- **Workflow:** disk-as-source-of-truth plans in `.coding/plans/<id>.md`
  (markdown checklists); in-memory `Workflow` state derived from the file.
  `Workflow::allowed_tools()` returns a `ToolFilter` applied before the request
  schema is built. `Planning` → read tools + `create_plan` + memory;
  `Executing` → all agent tools + `complete_step` + memory; `Complete` →
  read tools + memory.
- **Approval gate:** every mutating action goes through `needs_approval`
  (agent/approval.rs); file edits/writes produce a `similar`-based diff
  preview. The Tauri adapter holds the `oneshot::Sender` in `PendingApprovals`
  (ipc/approval.rs) keyed by `tool_call_id` and emits a serializable
  approval-request event; `approve` resolves the sender.
- **Sandbox:** `Sandbox::validate()` (canonicalize + starts_with) on all file
  agent tools. `.coding/memory.db` (and WAL/SHM) protected from direct write.
- **Memory:** SQLite + FTS5 + vectors (memory/). Four tiers
  (working/episodic/semantic/procedural); `recall` does keyword/semantic
  ranking + strength decay + access-count bumps; `consolidate_session`
  synthesizes episodic summaries. `HashEmbedder` default; `OllamaEmbedder`
  for real embeddings.
- **Safety:** regex-based auto-approve rules in `.coding/safety.toml`
  (`SafetyRules`), mtime-checked; a global `SafetyMode` toggle shared across
  agents. Constitution re-read from disk each turn (mtime-checked
  `ConstitutionSource`).
- **IPC surface (src-tauri/src/ipc/commands.rs):** prompt/suggestion/
  interrupt/cancel, approve, safety mode + rules CRUD, list/spawn agents,
  set_model, get_workflow_state, get_config, session/project stats + session
  list, read/list files, git branch, save/load conversation, and a backlog
  subsystem (add/list/remove/reorder/clear-finished/retry/auto-feed/
  dispatch-next/run-all/stop-all).
- **Frontend:** `useAgentStore` (Zustand) is the single source of UI state;
  `useAgentEvents` subscribes to Tauri events. Components split into layout/
  (Sidebar, MainPanel, RightPanel, StatusBar, InputBar, ConfigDialog,
  SafetyToggleDialog), chat/ (Conversation, Message [memoized], ToolCallCard
  via Message, ApprovalPrompt, DiffView, InflightBar), views/ (MdViewer,
  PlanProgress, DiffViewer, FileBrowser, ToolOutput, BacklogView, SafetyRules,
  StatsView). Slash commands in lib/slash.ts.

## Constitution rules the reviewers must respect

From `agent.md` (project): Windows 11, PowerShell syntax, project root
`C:\AgenticCoder\AgenticCoder`. Don't trust piped-command exit codes — read
`$LASTEXITCODE` unpiped. All public Rust fns need doc comments. Run `cargo
test` before marking a step complete. Never commit to main. The standard
plan-closing sequence is Test → Review (spawn_agent) → Act on findings →
Commit.

## Prior review round (for context, NOT to duplicate)

`reviews/01-architecture.md`, `reviews/02-quality.md`, `reviews/03-prd.md`,
`reviews/04-performance.md`, `reviews/00-consolidated-report.md`, and
`reviews/fix-plan.md` are a prior round. Many findings there were acted on
(e.g. per-agent workflows, shared `reqwest::Client`, constitution re-read,
tool-result truncation). This 2026 round is a fresh independent assessment of
the CURRENT code — verify against the source, do not assume prior findings
still apply.