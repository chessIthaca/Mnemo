# Architecture Quality Review — myharness

**Perspective:** Code quality (correctness, error handling, safety, concurrency, testing, production readiness)  
**Reviewer:** read-only architecture reviewer  
**Date:** 2026-04-08  
**Scope:** `src/` (brain), `src/safety_rules.rs`, `src-tauri/src/ipc/`, `frontend/src/`, `tests/`.  
**Method:** Deep sample of approval/dispatch/turn/workflow/sandbox/shell/git/run-all/event-forwarder; spot-check IPC, FE store tests, provider factory. No project source edited except this report.

---

## Executive summary

myharness remains a **strong, production-leaning agent harness**: typed errors, channel-decoupled multi-agent runtime, disk-backed plan stack with skill sidecar, hard workflow tool filtering at schema build time, `never_auto_for` core-op gating, and a carefully designed event forwarder that avoids holding the manager lock across `recv()`. Several prior quality debts (WorkflowStateInfo casing, provider key-resolution duplication, agent-loop file split) appear **fixed**.

The remaining quality bar is dominated by **safety/enforcement gaps that matter when the model is wrong or the user chooses a looser safety mode**, plus one **Run-All success/failure bookkeeping hazard**:

1. **Run-All can mark a failed turn successful** when `MAX_RETRIES` tool-error abort emits both a final `Error` and a subsequent `Finished` (forwarder treats each independently).
2. **Workflow gate is schema-only** — `ToolRegistry::dispatch` does not re-check `ToolFilter`, so a hallucinated/local-model tool call can mutate outside Planning intent.
3. **`shell` cwd is not sandbox-validated** and **shell has no execution timeout**.
4. **`AutoApproveProject` treats all `git` as project-scoped**, so `git commit` (full-tree `add -A`) auto-runs without a prompt; only merge/push force approval.
5. **Approval UI never receives Rust-side diffs** (`preview: None` always), despite `prepare_for_approval` existing on file tools.

**Overall verdict: Good core, not yet “unattended-safe.”** Interactive ApproveEachAction use is solid; Autonomous / AutoApproveProject / Run-All need hardening before trusting overnight batches.

---

## Quality scorecard

| Area | Rating | Notes |
|---|---|---|
| Correctness (core loop) | **B+** | Strong retry/sanitize paths; a few state-machine emission holes |
| Error handling | **A-** | `thiserror` library boundary; tool errors fed back to model; provider backoff |
| Safety & security | **C+** | Good sandbox + never_auto_for; gaps in shell cwd, filter enforcement, AutoApproveProject git |
| Concurrency | **A-** | Fan-in ownership correct; brief locks; approval cleanup agent-scoped |
| Testing posture | **B** | Excellent unit/integration on brain; Run-All orchestration + FE runner weak |
| API/schema consistency | **A-** | Prior FE/BE drifts largely repaired; event union solid |
| Observability | **B** | Events rich; many failures only `eprintln!` |
| Production readiness | **B** | Plan/stack resume strong; unattended git loop riskier |
| Constitution enforcement in code | **B+** | Plan-first gate real; core ops gated; bookkeeping AutoRun |

---

## Findings by severity

### Critical

#### C1. Run-All may commit work after a final tool-error abort
**Where:** `src/agent/turn.rs` (~682–699) emits `Error { retrying: false }` **and then** `Finished`; `src-tauri/src/ipc/events.rs` (~104–125, ~147–166) calls `on_main_turn_resolved(false, …)` on final Error **and** `on_main_turn_resolved(true, None)` on Finished when main is idle.

**Why it matters:** For the consecutive-tool-error abort path, the forwarder can:

1. Mark the backlog item `CantResolve` and **rollback** to the checkpoint.
2. Then treat the same turn as **success**, **commit** whatever remains (or the rolled-back clean tree + any racy writes), bump `done`, and dispatch the next item.

Even if rollback “wins” on disk, counters/status/notes become inconsistent; if commit races after further FS activity, failed agent work can be recorded as Done.

**Contrast:** Provider exhaustion in `run_turn_with_retry` (`src/runtime/agent.rs` ~93–103) emits only final `Error` (no `Finished`) — that path is consistent. The tool-error abort path is not.

**Recommendation:** Make terminal outcomes exclusive — either:

- emit final `Error` **without** `Finished`, and have the forwarder treat final Error as turn resolution; or  
- emit `Finished { reason: Error }` / a dedicated `TurnFailed` and resolve Run-All once; or  
- ignore `Finished` for Run-All if a final error was already seen this turn (per-agent latch).

Add an integration test: main agent hits `MAX_RETRIES` tool failures under Run-All → item `CantResolve`, tree at checkpoint, **no** success commit, loop halted or advanced only by explicit policy.

---

### High

#### H1. Workflow tool gate is not enforced at dispatch time
**Where:** Filter applied only when building schemas — `src/agent/turn.rs` (~204–205) via `ToolRegistry::schemas`; execution path `src/tool/mod.rs` `dispatch` (~284–294) runs any registered tool by name with **no** `ToolFilter` check. `execute_tool_call` (`src/agent/dispatch.rs`) likewise does not consult workflow state.

**Why it matters:** PLAN.md and constitution describe a **hard** plan-first gate (“write tools omitted”). Omission from the tools array is soft against:

- local/OpenAI-compatible models that invent tool names,
- prompt-injected tool-call JSON,
- future bugs that widen schemas.

A model in Planning could still call `file_write` / `shell` if it knows the name; the registry will execute after approval rules only.

**Recommendation:** In `execute_tool_call` (or `dispatch`), re-evaluate `workflow.allowed_tools().allows(...)` and return a tool error if denied. Unit-test Planning + Skill allow-list rejection.

#### H2. `shell` `cwd` is not sandbox-validated
**Where:** `src/tool/agent/shell.rs` ~86–100 — `cwd` is `project_root.join(c)` with **no** `Sandbox::validate` / normalize check.

**Why it matters:** `cwd: ".."`, `cwd: "../../somewhere"`, or absolute-ish joins can set the process working directory **outside** the project root. Combined with an approved (or Autonomous) shell command, this bypasses the path sandbox that file tools honor. Schema text says “relative to project root” but code does not enforce it.

**Recommendation:** Resolve + `Sandbox::validate` (or validate parent dir) before `current_dir`; reject escapes. Add tests for `..` and absolute outside paths.

#### H3. `shell` has no timeout / kill path
**Where:** `src/tool/agent/shell.rs` ~110 — unbounded `cmd.output().await`.

**Why it matters:** `sleep infinity`, stuck network tools, or interactive waits freeze the agent task (and stall Run-All resolution). Interrupt/Cancel do not appear to kill the child process mid-`output()`.

**Recommendation:** `tokio::time::timeout` + kill process group on expiry/interrupt; surface timeout as tool error. Same consideration for long `git` ops if needed.

#### H4. `AutoApproveProject` auto-approves all git ops except merge/push
**Where:** `src/agent/approval.rs` `is_project_scoped` — `"git" => true` (~112); `never_auto_for` only merge/push (`src/tool/agent/git.rs` ~167–177). `git commit` always runs `git add -A` then commit (~190–203).

**Why it matters:** In AutoApproveProject (and via safety-rule shortcut when not force-prompt), the agent can **stage and commit the entire dirty tree** without a user prompt — including secrets, unrelated WIP, or `.env` if not ignored. The comment that git “can’t touch files outside” the root is true but incomplete: commits are high-impact **inside** the root.

**Recommendation:** Treat write git subcommands (`commit`, `checkout`, `stash`, branch delete/create) as non-auto in AutoApproveProject, or force prompt for `commit` specifically. Prefer showing staged summary in approval preview.

#### H5. Approval previews are never populated
**Where:** `src/agent/dispatch.rs` ~107–113 sets `preview: None` always. `FileEditTool::prepare_for_approval` / `FileWriteTool::prepare_for_approval` exist but are unused on the live path.

**Why it matters:** PLAN.md promises **diff-based approval**. Users (and “Mark Safe”) decide on tool name + raw args only. Large destructive edits are harder to catch; FE may implement client-side diffs from args, but Rust-side `similar` preview is dead weight and can drift from actual execute path (e.g. line-ending normalization in execute but not in unused prepare).

**Recommendation:** Build preview in `execute_tool_call` before sending `ApprovalRequest` for file tools; keep preview generation pure (no write). Test preview matches post-normalize content where feasible.

---

### Medium

#### M1. `DenyAll` does not stop remaining tool calls in the same batch
**Where:** `dispatch.rs` returns a denied tool result for `DeniedAll` (~130–137); `turn.rs` continues the `for tc in &tool_calls` loop (~570+). Subsequent calls can still prompt or auto-run.

**Why it matters:** User intent of “deny all remaining” is only partially honored — they may get N−1 more prompts or silent auto-approvals under Autonomous.

**Recommendation:** Thread a `deny_all` latch for the rest of the turn (and optionally cancel further execution with synthetic denials).

#### M2. Incomplete `WorkflowStateChanged` emissions
**Where:** `turn.rs` ~655 only handles `create_plan` | `complete_step` | `abandon_plan`. Missing: `update_plan`, `skill_start`, `skill_end`, `abandon_skill`.

**Why it matters:** UI plan depth/skill badge/stack can lag until a polled `get_workflow_state`. Skill enter/exit especially affects available tools in the chrome.

**Recommendation:** Emit on any successful workflow-category tool (or centralize event emission inside workflow tools / a thin wrapper).

#### M3. Run-All halt-on-approval leaves ambiguous tree + status
**Where:** `halt_run_all_for_approval` (`commands.rs` ~2179–2195) sets item `Failed`, clears `run_all`, does **not** rollback. Agent may still run after the user later Approves.

**Why it matters:** Overnight run stops (good), but the working tree may contain partial uncommitted or later-committed work not tied to a clean backlog state. Checkpoint sha lives in `item.note` and is overwritten by the failure note.

**Recommendation:** Preserve checkpoint sha separately; on halt either rollback immediately or mark `PausedForApproval` and resume/rollback explicitly after user action.

#### M4. Success criterion for Run-All is “Finished without final Error,” not “plan Complete”
**Where:** `events.rs` Finished → `on_main_turn_resolved(true)` if main idle.

**Why it matters:** Agent can stop after partial work, or after MAX_RETRIES path issues (see C1), and still “succeed.” Unattended batch quality depends on model discipline + plan-first prompt, not a hard gate.

**Recommendation:** Optional strict mode: require workflow `Complete` (or explicit agent “done” signal) before commit_success.

#### M5. Protected write targets are narrow
**Where:** `Sandbox::is_protected_write_target` only blocks `.coding/memory.db` (+ wal/shm).

**Why it matters:** Approved `file_write` can corrupt `.coding/plans/stack.json`, plan markdown, `safety.toml`, backlog files — restart/resume and safety rules become attacker/model-writable.

**Recommendation:** Expand protected set (or make `.coding/**` writable only via dedicated tools). At minimum protect `stack.json`, `safety.toml`, `memory.db*`.

#### M6. `keys.toml` ACL is aspirational, not enforced in code
**Where:** PLAN.md claims ACL-restricted keys; `src/config/keys.rs` `save` uses atomic write only — no Windows ACL tightening.

**Why it matters:** Keys are plaintext on disk; multi-user or backup leakage risk. KeyStore correctly avoids Debug dumping keys (good).

**Recommendation:** On Windows, set user-only DACL after write; document permissions for other OS.

#### M7. Frontend store tests are not executed by a runner
**Where:** `frontend/src/hooks/useAgentStore.test.ts` header states no vitest/jest in package.json — tests are assertion functions for `tsc` only.

**Why it matters:** Regressions in event reducers (approval clear, exited cleanup, streaming) won’t fail CI.

**Recommendation:** Add vitest (or similar) and wire `npm test` into CI.

#### M8. Shell always reports `success: true` when the process spawns
**Where:** `shell.rs` ~122–131. Intentional for LLM interpretation.

**Why it matters:** `tool_error_count` / MAX_RETRIES only sees tool-framework failures, not command failures — a model can loop on failing builds forever within a turn until context/provider limits.

**Recommendation:** Acceptable with docs; optionally count high exit codes toward a softer “stuck” heuristic.

#### M9. Safety-rules reload swallows read errors and advances mtime
**Where:** `safety_rules.rs` `reload_if_changed` ~150–155.

**Why it matters:** Transient FS error after a bad edit can pin stale rules until next real mtime change. Low probability; worth logging at warn.

#### M10. `git commit -m` message is not validated for flag injection the way branch names are
**Where:** Branch names reject leading `-` (`valid_branch_name`); commit message is passed as `-m` + argv (generally safe as discrete argv). Low residual risk if more freeform args are added later.

**Recommendation:** Keep argv style; extend validation if new string args are introduced.

---

### Low

#### L1. Mutex poison → `expect` panic convention
Consistent across factory/loop/IPC. Fine for desktop agent; document as intentional (no recovery).

#### L2. Buffered non-Suggestion commands during approval/summarization only `eprintln!`
`turn.rs` ~598–600. Rare; Prompt buffered mid-approval could be surprising.

#### L3. Skill `ToolFilter::from_state(Skill) → Planning` defensive default
Safe only because `Workflow::allowed_tools` builds `Skill(list)` directly. Comment is good; a direct `from_state` misuse would be wrong.

#### L4. Observability mostly events + `eprintln!`
No structured log levels / correlation ids for multi-agent diagnosis. Acceptable for current desktop phase.

#### L5. `complete_with_retry` retries all provider errors indiscriminately
No distinction 401 vs 429 vs 500. May delay hard auth failures.

#### L6. Public doc-comment rule
Prior gap (`get_config`) may be fixed; spot-check still recommended on new IPC commands in the large `commands.rs`.

---

## Testing gap analysis

### Strong today
| Area | Evidence |
|---|---|
| Workflow lifecycle / stack / skills | `src/workflow/mod.rs` tests; `tests/workflow_integration.rs` |
| Approval matrix + project-scope | `src/agent/approval.rs` tests |
| Sandbox path traversal | `src/tool/agent/sandbox.rs` tests |
| never_auto_for merge/push | `src/tool/agent/git.rs` tests |
| SSE / build_request_json | `src/provider/openai.rs` tests |
| PendingApprovals multi-agent cleanup | `src-tauri/src/ipc/approval.rs` tests |
| Run-All git checkpoint/rollback helpers | `src-tauri/src/ipc/run_all.rs` tests |
| IPC event serialization | `tests/ipc_bridge.rs` |
| Safety rule signatures / is_safe | `src/safety_rules.rs` tests (improved vs older review) |
| Agent loop malformed JSON / retries | `src/agent/tests.rs` |

### Weak / missing
| Gap | Risk |
|---|---|
| Run-All **orchestration** (`on_main_turn_resolved`, Error+Finished ordering, halt_for_approval) | **C1**, M3 — highest unattended risk |
| Dispatch-time ToolFilter enforcement | **H1** |
| Shell cwd escape + timeout | **H2**, **H3** |
| Approval preview wiring E2E | **H5** |
| `set_model` live swap across factory + running loops | config drift / wrong client mid-session |
| spawn_agent → parent Suggestion → ChildFinished E2E through real forwarder | multi-agent UX correctness |
| Provider live stream (still `#[ignore]`) | timeout/decode paths |
| Frontend automated UI/store tests in CI | reducer regressions |
| FE tests file exists but **does not run** | M7 |

### Flaky patterns to avoid
- Run-All tests that share a real repo path or global git config (existing `TestRepo` isolation is good — keep it).
- Timing-based “agent finished” sleeps (constitution already forbids agent sleep/poll; tests should use channels/barriers).

---

## Safety / security notes

### What works well
- File tools canonicalize + `starts_with(root)`; creation path has lexical normalize then re-validate.
- Memory DB protected from `file_write`.
- `shell` and unknown tools are **not** project-scoped under AutoApproveProject (conservative default).
- Core ops `git merge` / `git push` force interactive approval even in Autonomous and even with matching safety rules (`force_prompt` in `dispatch.rs`).
- Git args passed as argv (not shell strings); branch flag-injection partially blocked.
- Approval oneshots never cross IPC; pending map cleanup is per-agent (regression test for prior whole-map clear bug).
- Run-All **does not auto-approve** — approval halts the batch (policy correct; implementation details in M3).
- Keys not Debug-printed; client factory centralizes key resolution (prior drift hazard fixed).

### Residual threat model (desktop, trusted user, untrusted model)
| Surface | Note |
|---|---|
| Prompt injection → tool calls | Expected; mitigated by approval modes + should be mitigated by dispatch filter (H1) |
| Autonomous mode | Full trust — document as “model can run shell as you” |
| Safety.toml user regexes | Powerful; a broad `shell:.*` rule is foot-gun by design |
| IPC commands | No auth beyond “local Tauri webview” — correct for single-user desktop; don’t expose the same commands over network later without an auth layer |
| Secrets in repo via `git add -A` | H4 / commit staging |
| Shell as universal escape | Even with sandbox files, approved shell bypasses FS sandbox entirely — correct, but cwd must not widen it further (H2) |

### AutoApproveProject static analysis gaps (explicit)
| Tool | Static check | Gap |
|---|---|---|
| file_* | path via sandbox | OK |
| search | always in-root walk | OK (read-only) |
| git | always true | **commit/checkout auto** (H4) |
| shell | always false | OK for scope; still needs cwd fix when approved |
| spawn_agent | falls through false | OK (needs approval) |
| other | false | OK |

---

## Strengths

1. **Clear error topology** — library `thiserror` + binary `anyhow`; tool failures become model-visible tool messages; provider retries layered (stream establish vs mid-stream vs turn-level).
2. **Concurrency discipline** — event forwarder owns fan-in rx; manager lock only for short mutations; approval wait is `select!`-interruptible; subagent cleanup Cancels under one lock with `try_send` FIFO rationale documented.
3. **Durable workflow** — plans on disk, `stack.json` with skill overlay + legacy array compat, empty-step rejection, stepless md skip on resume.
4. **Constitution reflected in code** — plan-first schema filter, bookkeeping AutoRun, core git ops `never_auto_for`, Windows PowerShell shell tool.
5. **Malicious/broken model resilience** — malformed tool JSON sanitized before re-send; consecutive error cap; mid-stream partial output handling with retry notes.
6. **Prior quality debts addressed** — WorkflowStateInfo serde casing + depth/parents; `client_factory` single key chain; agent modules split (`turn`/`dispatch`/`loop_impl`); run_all helper tests exist.
7. **Multi-agent approval isolation** — `(agent_id, tool_call_id)` map + `cleanup_for_agent` tests.

---

## Prioritized quality improvements (recommendations only)

1. **Fix Run-All terminal event exclusivity (C1)** — highest unattended correctness risk; add failing test first.
2. **Enforce ToolFilter at dispatch (H1)** — closes plan-first hard-gate hole.
3. **Sandbox shell cwd + add timeout/kill (H2, H3).**
4. **Narrow AutoApproveProject git auto-approval; wire approval previews (H4, H5).**
5. **DenyAll latch for remainder of tool batch (M1).**
6. **Emit WorkflowStateChanged for all workflow/skill tools (M2).**
7. **Harden Run-All halt/checkpoint metadata (M3); optional Complete-gated success (M4).**
8. **Expand protected `.coding` paths (M5); keys file ACLs (M6).**
9. **Install FE test runner + CI (M7); add Run-All orchestration tests next to helper tests.**
10. **Structured logging for forwarder/backlog git failures** (replace silent `eprintln` for rollback/commit failures).

---

## Open questions

1. Should **Autonomous** mode still allow unrestricted `shell`, or require an explicit second “I understand” config flag for unattended batches?
2. Is Run-All success defined as **agent Finished**, **plan Complete**, or **clean git tree + tests green**? Product answer drives M4.
3. When approval halts Run-All mid-item, should the default be **rollback now** or **leave WIP for the user**?
4. Are skill allow-lists meant to be able to include `shell`/`git commit` without extra prompts when safety mode is Autonomous?
5. Should subagent final Error notify parent and also affect parent Run-All resolution if the parent is waiting on children? (Today descendants_running gates main Finished — good — but child failure still notifies via Suggestion only.)
6. Is there an intentional reason approval `preview` was left `None` (FE-only diffs), or is it unfinished wiring?

---

## Constitution compliance (code vs rules)

| Rule | Code reality |
|---|---|
| Plan-first before writes | Schema filter yes; **dispatch re-check no** (H1) |
| Core ops always approval-gated | **Yes** for merge/push via `never_auto_for` + `force_prompt` |
| Bookkeeping tools AutoRun | **Yes** (memory + plan lifecycle SafetyLevel) |
| Public fn doc comments | Largely honored; keep enforcing on new IPC |
| Windows paths / PowerShell | Shell tool correct; agent.md enforces for the coding agent |
| Never sleep-wait on subagents | Prompt rule; forwarder completion notification supports it |
| Line-ending preservation | Implemented in file tools |

---

*Report complete. Read-only review; only this file under `.coding/reviews/` was written.*
