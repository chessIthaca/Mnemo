# PRD Review — myharness implementation vs `PLAN.md`

**Date:** 2026-04-04
**Reviewer:** PRD-perspective reviewer (read-only subagent)
**Spec:** `PLAN.md` (root). **Baseline:** `.coding/reviews/2026-review-baseline.md`.
**Scope:** the entire myharness codebase — Rust brain (`src/`), Tauri shell + IPC bridge
(`src-tauri/`), React frontend (`frontend/`), tests (`tests/`). Build/test verified green
this session: `cargo test` = 400 lib + 9 ipc_bridge + 5 workflow_integration, exit 0.

## Executive summary

The Rust "brain" matches `PLAN.md` with high fidelity. Every one of the six critical
gotchas is actually handled in code, the enforced plan-first workflow (write tools
*omitted* from the `tools` array in Planning) is implemented exactly as the PRD states and
proved by an integration test, the channel contract + oneshot-approval IPC bridge match the
spec, and disk-as-source-of-truth plan resume works. The **deviations are concentrated in
the frontend dependency stack**: the locked-in decisions table specified **Tailwind v4,
shadcn/ui (Radix), Shiki, and react-diff-view + refractor** — the implementation uses
**Tailwind v3, no shadcn/Radix, no Shiki, and a hand-rolled LCS diff** instead of
react-diff-view. These are real, evidence-backed deviations from locked-in decisions, not
gaps in behavior. Several features **exceed** the spec (vision fallback, spawn_agent tool,
backlog + overnight Run-All loop, stats, safety-rules UI, sub-plan stack).

**Overall verdict: Matches-with-frontend-deviations.** The architecture and brain are
closely-matched; the frontend met the *functional* exit criteria of phases A–E but not the
locked-in *technology* choices for rendering/highlighting/diff/components.

---

## Area-by-area assessment

### 1. Locked-in technical-decisions table

| Concern | PLAN.md choice | Reality | Verdict |
|---|---|---|---|
| GUI framework | Tauri 2.x | `src-tauri/Cargo.toml` uses `tauri = "2"`; `@tauri-apps/api ^2` in `frontend/package.json` | ✅ Match |
| Frontend | React 18 + TS + Vite | `react ^18.3.1`, `typescript ^5.6.3`, `vite ^5.4.21`, `tsconfig.json` present | ✅ Match |
| Styling | **Tailwind v4** | `tailwindcss ^3.4.15` + `autoprefixer ^10` + `postcss ^8` (`frontend/package.json:29`); `tailwind.config.ts` uses the v3 shape (`darkMode:"class"`, `theme.extend`) | ❌ **Deviation** — v3, not v4 |
| Component library | **shadcn/ui (Radix + Tailwind)** | No `@radix-ui/*` deps, no `components.json`, no `components/ui/` shadcn folder; all components hand-built | ❌ **Deviation** — shadcn/ui absent |
| Markdown | react-markdown + remark-gfm + rehype-highlight | `react-markdown ^9`, `remark-gfm ^4`, `rehype-highlight ^7`; used in `Message.tsx:2-4` + `MdViewer.tsx:2-4` | ✅ Match |
| Code highlighting | **Shiki** | Not installed; `rehype-highlight` (highlight.js) is used instead | ❌ **Deviation** — Shiki absent (highlight.js substitute) |
| Diff rendering | **react-diff-view + refractor** | Not installed; custom LCS diff in `frontend/src/components/chat/DiffView.tsx:22` (no syntax highlighting in diffs) | ❌ **Deviation** — hand-rolled LCS, no refractor |
| Icons | lucide-react | `lucide-react ^0.460.0` | ✅ Match |
| Async runtime | tokio | `tokio` in brain + `src-tauri` | ✅ Match |
| LLM client | async-openai | `async-openai` (`src/provider/openai.rs:10`) | ✅ Match |
| Provider strategy | one client, swap base URL | `OpenAiClient` swaps `base_url` via `OpenAiClientConfig`; `ProviderKind::{OpenAI,Local}` (`provider/mod.rs:79-82`) | ✅ Match |
| Streaming tool calls | parse deltas, correlate by `index` | `DeltaAccumulator` keyed `HashMap<u32, ToolCallAccumulator>` (`provider/stream.rs:44`) | �� Match |
| Diff library (Rust) | similar | `similar::TextDiff` unified diff in `tool/agent/file_edit.rs:12,61-64` | ✅ Match |

**Verdict: ⚠️ Partial / Deviation.** 10 of 13 rows match; 3 locked-in frontend libraries
(Shiki, react-diff-view+refractor, shadcn/ui) and the Tailwind major version are deviated.
The deviations are functional substitutes, not missing features (see "Deliberate deviations").

### 2. The six critical gotchas

1. **Ollama has no `tool_choice`** — ✅ Match. `Capabilities::local()` sets
   `supports_tool_choice: false` (`provider/mod.rs:60`); `openai.rs:455` gates
   `body["tool_choice"]` behind `self.caps.supports_tool_choice`; test
   `local_client_omits_tool_choice` (`openai.rs:1304`) asserts it's omitted.
2. **`finish_reason` unreliable** — ✅ Match. Tool calls detected by *presence of deltas*
   via `DeltaAccumulator::saw_tool_calls()` (`provider/stream.rs:47,74`); test
   `detects_tool_calls_by_delta_presence_not_finish_reason` (`stream.rs:160`).
3. **Malformed tool-call JSON** — ✅ Match. `agent/mod.rs:732-734` validates every call's
   `arguments` with `serde_json::from_str`; bad JSON feeds an error tool message + retries,
   sanitizing args to `"{}"` so the gateway doesn't 400 (`mod.rs:765-800`); test
   `error_recovery_malformed_json` (`mod.rs:1430`).
4. **Fragmented argument deltas by index** — ✅ Match. `ToolCallAccumulator` per-`index`
   (`stream.rs:13-39`); `finalize()` sorts by index (`stream.rs:79-86`); test
   `accumulates_multiple_tool_calls_by_index` (`stream.rs:121`).
5. **oneshot not serialized across IPC** — ✅ Match. `AgentEvent::ApprovalRequest` carries
   `oneshot::Sender` (`channels.rs:66-72`); `into_serializable()` extracts it
   (`channels.rs:219-233`); `PendingApprovals` holds it keyed by `tool_call_id`
   (`src-tauri/src/ipc/approval.rs:17-53`); `approve` command resolves it
   (`commands.rs:162-168`); round-trip test (`tests/ipc_bridge.rs:42-90`).
6. **`reasoning_content` → ReasoningDelta** — ✅ Match. `openai.rs:583` reads
   `delta["reasoning_content"]` and emits `LlmEvent::ReasoningDelta` (`openai.rs:585`);
   loop forwards as `AgentEvent::ReasoningDelta` (`agent/mod.rs:521-524`); test
   `parse_sse_chunk_reasoning_content` (`openai.rs:964`).

**Verdict: ✅ Match** — all six gotchas handled, each with a test.

### 3. Capability-aware provider strategy

✅ Match. `Capabilities` struct (`provider/mod.rs:24-41`) has all five PRD fields
(`supports_tool_choice`, `supports_strict_schema`, `supports_parallel_tools`,
`reliable_finish_reason`, `max_context`) **plus two extensions** (`max_output_tokens`,
`multimodal`). `ProviderKind::{OpenAI,Local}` with `capabilities()` (`mod.rs:79-91`).
The loop queries `provider.capabilities()` before building the system prompt
(`agent/mod.rs:467`) and tool schemas (`mod.rs:492`); `schemas()` only sets
`strict = Some(true)` when `caps.supports_strict_schema` (`tool/mod.rs:217-221`); test
`strict_only_when_caps_allow` (`tool/mod.rs:459`). Endpoint config supports `max_context`,
`max_output_tokens`, `multimodal`, `reasoning_effort` overrides
(`config/endpoints.rs:50-74`). `--provider`/runtime switch via `set_model` command + store.

### 4. Enforced plan-first workflow (tool-filter rules)

✅ Match — **exactly** as PLAN.md states. PLAN.md: *"Planning → read agent tools +
create_plan + all memory tools. Write agent tools, shell, and complete_step are omitted
from the tools array."* `ToolFilter::Planning` allows `ToolCategory::Agent` only when
`safety == SafetyLevel::AutoRun` (`tool/mod.rs:134`) — i.e. only read tools; all mutations
(file_edit/write/append, shell, git, spawn_agent) are `NeedsApproval` and thus **hidden**,
not merely approval-gated. The comment at `mod.rs:129-133` states the intent: *"Hiding
(not just approval-gating) makes the rule unskippable."* `Executing` → all agent tools +
`complete_step`/`create_plan`/`update_plan`/`abandon_plan` + memory (`mod.rs:141-158`);
`Complete` → read tools + `create_plan` + memory (`mod.rs:159-169`). Integration test
`planning_gate_hides_write_tools_until_a_plan_exists` (`tests/workflow_integration.rs:98`)
asserts `file_write` is absent from the schema in Planning (`:114-115`).

**Note (not a deviation):** the agent loop calls `provider.complete(..., None)` for
`tool_choice` (`agent/mod.rs:1125`) — it never forces a tool. PLAN.md doesn't require
forcing tool_choice in any state, so this is fine.

### 5. Disk-as-source-of-truth plans; resume on restart

✅ Match. Plans live at `.coding/plans/<id>.md` as markdown checklists
(`workflow/plan_file.rs`). `Workflow::load_latest()` (`workflow/mod.rs:349`) rebuilds the
stack from a `stack.json` sidecar (or latest `.md` fallback), parses checkboxes, and
**derives** state: unchecked→Executing, all-checked→Complete, none→Planning (`mod.rs:391-396`);
resume point = first unchecked step (`current_step`, `mod.rs:119`). Tests:
`load_latest_resumes_executing` (`mod.rs:541`), `load_latest_skips_stepless_plan_files`
(`mod.rs:445`), `completing_sub_plan_pops_and_resumes_parent` (`mod.rs:618`). Sub-plan
stack (`create_plan` while executing pushes) is an extension beyond the PRD's single-plan
model but is consistent with "disk source of truth."

### 6. Context management + truncation

✅ Match. `ContextManager` counts tokens via `try_tiktoken_count` (tiktoken-rs) with
`char_based_estimate` (~4 chars/token) fallback (`context.rs:47-54,245-275`) — exactly
"tiktoken for OpenAI, char-estimate for local." Summarize-at-fill-rate: `summarize_at =
max_tokens * fill_rate` (`context.rs:28-29`), default 50% (test `:333` shows 0.5→64K of
128K). **"Large tool results are truncated with a note"** is implemented at the tool level:
`file_read` caps at 2000 lines / 100 KB and appends `"... (truncated: N lines total,
showed M)"` (`tool/agent/file_read.rs:17-23,130-145`); `search` caps output with a note
(`tool/agent/search.rs:320`). The agent loop itself does not re-truncate (it passes
`result.output` straight into the tool message, `agent/mod.rs:897`), but the tools
self-truncate, satisfying the PRD's intent. Tests: `truncates_large_file_by_line_count`
(`file_read.rs:223`), `truncates_very_long_lines_by_bytes` (`file_read.rs:251`).

### 7. Error recovery

✅ Match. Every tool result (success or error) is fed back as a `Role::Tool` message
(`agent/mod.rs:894-902`). Malformed JSON / unknown tool / empty-response errors also become
tool-role messages (`mod.rs:787-800`, `dispatch` error in `tool/mod.rs:228-237`).
`MAX_RETRIES = 3` (`agent/mod.rs:38`); consecutive-tool-error cap aborts + emits
`AgentEvent::Error` (`mod.rs:737-748, 944-950`). Provider errors retry with exponential
backoff **1s, 2s, 4s** in `complete_with_retry` (`mod.rs:1122-1134`,
`delay_ms=1000` then `*=2`); test `complete_with_retry_should_try_3_times` (`mod.rs:2274`).

### 8. Path safety (sandbox)

✅ Match. `Sandbox::validate()` (`tool/agent/sandbox.rs:50`) canonicalizes existing paths
(catches symlinks + `..` traversal) and `starts_with(root)` checks inside
(`check_inside`, `:109-115`); non-existent paths canonicalize the parent then append the
name (`:64-74`). Absolute-outside-root rejected (test `:203`). `validate_for_creation`
lexically normalizes `..` for `file_write` dir creation (`:93-106`). `.coding/memory.db`
+ `-wal`/`-shm` protected from direct write (`is_protected_write_target`, `:120-127`).
Each file tool routes through `sandbox.validate()` (e.g. `file_read`, `file_edit`,
`file_write`, `file_append`, `search`, `describe_image`). PLAN.md: *".coding/ writable;
rest read/write; nothing outside root"* — `.coding/` is writable (plans, memory.db via the
store, safety.toml); the rest read/write; nothing outside root accessible.

### 9. Two config scopes + two agent.md files

✅ Match (with one deliberate, reasonable deviation — see below). Global config in
`~/.myharness/` (`config.toml`, `endpoints.toml`, `keys.toml`, `projects.toml`) loaded by
`Config::load` (`config/mod.rs:46-58`); per-project `.coding/config.toml` override path
defined (`project/mod.rs:34,51`). Two `agent.md` files loaded **global first, then
project** (`project/agent_md.rs:11-18`) and **re-read each turn** via mtime-checked
`ConstitutionSource` (`agent_md.rs:70-101`); the loop reads `self.constitution.constitution()`
each turn (`agent/mod.rs:463`). Tests: `source_rereads_after_mtime_change` (`agent_md.rs:216`).

**Deviation (deliberate, reasonable):** PLAN.md's directory diagram places the project
constitution at `<project-dir>/.coding/agent.md`, but the implementation puts it at
`<project-dir>/agent.md` (project root): `agent_md: root.join("agent.md")`
(`project/mod.rs:47`). The actual `agent.md` is at `C:\AgenticCoder\AgenticCoder\agent.md`.
The code comment (`mod.rs:24-26`) explains it: kept at the root "so it's visible and easy
to edit." This is a spec-diagram mismatch, not a behavioral gap — update the PLAN.md
diagram to match.

### 10. Multi-agent runtime + channel contract

✅ Match. `AgentCommand` has all four PRD variants — `Prompt`, `Suggestion`, `Interrupt`,
`Cancel` (`runtime/channels.rs:27-39`; `Prompt` carries `images` — an extension for
attachments). `AgentEvent` has every PRD variant — `Started`, `TextDelta`,
`ReasoningDelta`, `ToolCallStart`, `ToolCallArgDelta`, `ApprovalRequest` (with oneshot),
`ToolResult`, `WorkflowStateChanged`, `StepCompleted`, `Finished`, `Error`
(`channels.rs:47-148`) — **plus extensions**: `Usage`, `ContextUsage`,
`SuggestionInjected`, `Exited`, `ChildFinished`. `AgentManager` spawns/routes/fans-in
(`runtime/`); per-agent `Workflow` independence via `AgentLoopFactory`
(`agent/factory.rs`, test `per_agent_workflows_are_independent_via_factory`).

### 11. Tauri IPC bridge

✅ Match. Every PRD-listed command exists in `src-tauri/src/ipc/commands.rs`:
`send_prompt` (`:111`), `send_suggestion` (`:125`), `interrupt` (`:138`), `cancel`
(`:150`), `approve` (`:162`), `spawn_agent` (`:238`), `list_agents` (`:214`),
`get_workflow_state` (`:492`), `save_conversation` (`:783`), `load_conversation`
(`:826`), `read_file` (`:622`), `list_files` (`:637`), `get_config` (`:588`).
**Extras (beyond PRD):** safety-mode get/set + safety-rules CRUD, `set_model`,
`get_session_stats`/`get_project_stats`/`get_session_list`, `get_git_branch`,
`list_markdown_files`, and the whole backlog subsystem (`backlog_*`, `run_all`).
Rust→frontend events via a single channel (`AGENT_EVENT_CHANNEL`, `events.rs:198`); the
forwarder calls `into_serializable()`, stores the oneshot in `PendingApprovals` for
`ApprovalRequest` (`events.rs:178`), emits `AgentEventPayload{agent_id,event}`
(`events.rs:315-320`). No PRD-listed command/event is missing.

### 12. UI layout regions + right-panel views

✅ Match. All five regions present in `App.tsx:193-203`: `Sidebar` (agent switcher),
`MainPanel` (tabbed conversation), `RightPanel` (tabbed views, on-demand via
`rightPanelVisible`), `InputBar` (textarea + history + slash), `StatusBar`
(model/provider/workflow/tokens/branch). All five right-panel views exist:
`MdViewer.tsx`, `PlanProgress.tsx`, `DiffViewer.tsx` (+ inline `DiffView.tsx`),
`ToolOutput.tsx`, `FileBrowser.tsx` (baseline inventory lines 122-129). Diff viewer
auto-shows on approval (the `DiffView` is embedded in `ApprovalPrompt` and the right
panel). File browser → md viewer handoff present (`FileBrowser.tsx`).

### 13. Build phases A–E exit criteria

- **Phase A (Tauri scaffold + IPC bridge):** ✅ Met. Workspace (`Cargo.toml` lib +
  `src-tauri`), IPC adapter, `Serialize` on `AgentEvent`/`ToolResult`/`Approval`/
  `ApprovalPreview`/`FinishReason`/`WorkflowState` (`channels.rs`),
  `PendingApprovals` oneshot map, `main.rs` builder. Exit test: headless round-trip
  (`tests/ipc_bridge.rs` 9/9) covers serialization + approval oneshot resolution.
- **Phase B (frontend foundation + chat):** ✅ Met. Layout components, `tauri.ts`
  wrappers, `useAgentEvents` subscribes to events, Zustand store, streaming text.
- **Phase C (rich rendering):** ⚠️ Partial. Markdown + GFM + highlight.js code blocks
  render (`Message.tsx`); tool-call cards + collapsible results present; file-edit
  approvals show a diff (custom LCS, not react-diff-view). **Not met:** the locked-in
  *Shiki* highlighting and *react-diff-view+refractor* diff rendering were substituted
  (see Area 1). Functional exit criteria ("markdown renders… diffs show… cards render")
  are satisfied, but the specified libraries are not.
- **Phase D (right-panel views):** ✅ Met. All five views render; right panel toggles and
  hosts them; file browser → md viewer works.
- **Phase E (polish + slash + theming):** ✅ Met. Slash commands `/clear /model /provider
  /help /save /load /panel` (`lib/slash.ts`); dark default + light toggle persisted
  (`useAgentStore.ts` `applyTheme`, `LS_THEME`); conversation save/load commands wired
  (`commands.rs:783,826`). Phase E's "surface provider errors gracefully" handled via the
  `Error{retrying}` event.

### 14. Constitution hard rules (in code)

- **Doc comments on all public fns:** ✅ honored in practice — spot-checked `provider/mod.rs`,
  `tool/mod.rs`, `workflow/mod.rs`, `agent/mod.rs`, `sandbox.rs`, `agent_md.rs`; every
  `pub fn`/`pub struct`/`pub enum` has a `///` doc comment. `cargo test --doc` clean.
- **Windows / PowerShell:** n/a for source code; the constitution is a runtime instruction
  to the agent, not enforced in compiled code.
- **Plan-first workflow:** ✅ enforced in code (Area 4) — it is a hard gate, not a prompt
  instruction, exactly as `agent.md` requires.
- **Never commit to main:** this is a process rule, not a code rule. Current branch is
  `feat/reviewer-report-instructions` (verified via `git branch`), not main. Honored.

**Verdict: ✅ Match.**

---

## Deliberate deviations (update the spec)

These are reasonable engineering choices where reality diverges from a locked-in PLAN.md
row. The spec should be amended to match the implementation (or the libs adopted):

1. **Tailwind v3, not v4** (`frontend/package.json:29`, `tailwind.config.ts`). v3 with
   `autoprefixer`+`postcss` is the standard v3 toolchain; v4 drops those. Either upgrade to
   v4 or update PLAN.md's row to "Tailwind v3."
2. **No shadcn/ui / Radix.** All UI components are hand-built Tailwind components; there is
   no `components/ui/` shadcn folder or `@radix-ui/*` dependency. Functionally the "modern,
   accessible" goal is met, but the locked-in component library is absent. Update the spec
   or adopt shadcn/ui.
3. **No Shiki.** Code highlighting uses `rehype-highlight` (highlight.js) everywhere
   (`Message.tsx`, `MdViewer.tsx`). PLAN.md lists Shiki as the code-highlighting choice and
   notes it's "upgradeable" from highlight.js — so the current state is the *pre-upgrade*
   baseline. Document this explicitly.
4. **No react-diff-view + refractor.** Diffs use a hand-rolled LCS renderer
   (`DiffView.tsx:22`) without syntax highlighting. The Rust-side `similar` diff *is*
   present (for the approval preview payload, `file_edit.rs:60`). Update the spec's
   "Diff rendering" row or adopt react-diff-view.
5. **Project `agent.md` location.** PLAN.md diagram shows `.coding/agent.md`; code puts it
   at the project root (`project/mod.rs:47`). Update the diagram to `<root>/agent.md`.

## Real gaps (implement these)

None that break functionality. The only item that could be called a "gap" relative to the
PRD's *stated technology* is the trio of absent frontend libraries (Shiki, react-diff-view,
shadcn/ui) + Tailwind v4 — but these are covered under "Deliberate deviations" since
working substitutes exist and the functional exit criteria are met. If strict adherence to
the locked-in table is required, those four are the action items.

No behavioral PRD requirement is unimplemented:
- Plan-first gate ✅, resume ✅, truncation ✅, error recovery ✅, sandbox ✅, two-config ✅,
  multi-agent channels ✅, IPC bridge ✅, all UI regions/views ✅, all phases' functional
  exit criteria ✅.

## What exceeds the spec

Features present in the codebase that `PLAN.md` does not mention:
- **Vision fallback** — `VisionClient` + `describe_image` tool + image-attachment fallback
  when the main LLM isn't multimodal (`provider/vision.rs`, `tool/agent/describe_image.rs`).
  `Capabilities.multimodal` + endpoint `multimodal` flag.
- **`spawn_agent` tool** — agent spawns background sub-agents via `AgentSpawner`
  (`tool/agent/spawn_agent.rs`); `ChildFinished` event + parent-aware spawning. Not in the
  PRD's tool list.
- **Extra workflow tools** — `file_append`, `update_plan`, `abandon_plan` (sub-plan stack).
- **Backlog subsystem + overnight Run-All loop** — `src-tauri/src/ipc/backlog.rs`,
  `run_all.rs`: add/list/remove/reorder/clear/retry/auto-feed/dispatch/run-all/stop-all,
  git checkpoints per item, approval-halts-the-loop semantics. Entirely beyond PLAN.md.
- **Safety-rules system** — regex auto-approve rules in `.coding/safety.toml`
  (`src/safety_rules.rs`), mtime-checked, with a Safety tab editor + "Mark Safe" action +
  global `SafetyMode` toggle (4 modes incl. `Autonomous`). PLAN.md only mentions an
  "approve-each-action gate"; this is a rich superset.
- **Stats / pricing** — `RequestStats` recorded to memory.db (`agent/mod.rs:594-617`),
  `get_session_stats`/`get_project_stats`/`get_session_list` commands, `StatsView.tsx`,
  per-model pricing in `endpoints.toml`.
- **Richer `AgentEvent`** — `Usage`, `ContextUsage`, `SuggestionInjected`, `Exited`,
  `ChildFinished` beyond the PRD's enum.
- **Sub-plan stack** — nested plans with `stack.json` sidecar for full-nesting resume.
- **Reasoning-effort control** + `max_output_tokens` endpoint overrides + cached-token
  heuristic for cost estimation.

## Top spec-conformance actions

1. **Reconcile the frontend dependency stack with PLAN.md's locked-in table.** Decide per
   row: either upgrade (Tailwind v4, Shiki, react-diff-view+refractor, shadcn/ui) or amend
   PLAN.md to record the actual choices (Tailwind v3, highlight.js, hand-rolled LCS diff,
   hand-built components). This is the single largest spec-vs-reality gap.
2. **Update PLAN.md's directory diagram** to show `<root>/agent.md` (not `.coding/agent.md`)
   to match `project/mod.rs:47`.
3. **Document the beyond-spec features** (vision fallback, spawn_agent, backlog/Run-All,
   safety-rules, stats, sub-plan stack) in PLAN.md so the spec reflects the shipped product
   — otherwise the PRD understates the implementation.
4. **Optional:** the agent loop always passes `tool_choice = None` (`agent/mod.rs:1125`).
   PLAN.md's `ToolChoice` enum and "force a tool" capability are implemented in the client
   but never exercised by the loop. If forcing tools in a given workflow state is desired,
   wire it; otherwise note it as an unused capability. (Not a gap — the PRD doesn't require
   forcing tool_choice in any state.)

---

*This reviewer created/edited only `.coding/reviews/2026-prd-review.md` and changed no
other project files. All evidence was verified against current source this session.*