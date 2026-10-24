# Mnemo2 — Lead + Team (design v0.2)

Status: Phase 0b conversation-first prototype. This is the design of record for
the Mnemo2 frontend rebuild. It supersedes the earlier "spaces dashboard"
sketch: Mnemo2 is a single-project cockpit for a lead agent and a team of
worker subagents. The defining change in v0.2 is that the lead enforces a
workflow **per task**, tasks run in parallel, and the interface is organized
around conversations rather than a global run diagram. The companion policy
change is in `agent.md` under **Mnemo2 team merge policy**.

## 1. Product shape

Mnemo2 is a **single-project desktop cockpit**. The start screen chooses one
project directory; the running instance is scoped to it. Working on two
projects means two instances (later: two windows).

- **Start screen** — recent projects from `projects.toml`, open folder, create
  project. Shows the branch, last session, and unfinished worker branches from
  a previous run with Resume / Review / Clean. The project picker is a compact
  list, not a marketing page.
- **Lead chat** — the main conversation: planning, guidance, research, review,
  and merge records. It is task-oriented: delegation and status appear as
  compact cards tied to specific tasks. The lead never streams worker tool logs
  into this chat and there is no global run timeline.
- **Team board** — the per-project Mission Control: peek sheet + control room.
  It is a conversation-first task list, not a phase dashboard. Every row shows
  the task's model and its own workflow state.
- **Worker thread** — the primary view of one task: the real conversation with
  that worker, its branch/diff/review, its model, and its own workflow. Worker
  tool logs stay here.
- **Workbench** — Browser, Diff, and Plan tabs. The browser is a native child
  webview on Windows (macOS fallback), not an iframe.

### 1.1 UI principles

1. **Conversation first.** The lead and worker threads are the product. Cards
   exist only to summarize a decision, a delegation, a review, or a merge in a
   few lines; the full reasoning lives in the conversation.
2. **Per-task workflow.** Each delegated task carries its own workflow, owned
   and enforced by the lead. There is no project-wide pipeline or "Run 04"
   diagram.
3. **Parallel by default.** Tasks advance independently. The team view groups
   and filters tasks by their own state; it never implies that every task must
   move through the same global phase at the same time.
4. **Progressive disclosure.** Show the current state and the one action that
   matters. Put timeline, checks, findings, and history behind a tab or a
   disclosure, not in the first screenful.
5. **Model identity is primary.** The lead's model and every worker's model are
   visible wherever that agent appears: task rows, conversation headers,
   delegation cards, and merge records. Choosing a model is a product decision,
   not hidden telemetry.
6. **Minimal explanatory copy.** Do not explain the metaphor in the UI. Labels,
   state, and the conversation carry the meaning. Empty space and quiet type are
   preferable to helper paragraphs.

### 1.2 Task workflow contract

The workflow belongs to the task, not to the application. The lead creates it
when it delegates, may revise it when scope changes, and is the only agent that
can advance Review or Merge.

```
Brief → Implement → Verify → Review → Merge
```

- **Brief** — the lead writes a self-contained brief: plan excerpt, paths,
  acceptance criteria, scope limit, model choice, and check scope.
- **Implement** — the worker edits in its worktree and commits. It may message
  the lead for a decision, but it does not replan.
- **Verify** — the check scope runs on the branch tip. A fix commit invalidates
  the previous result; the affected scope must pass again.
- **Review** — the lead reviews the delta and records findings. Every finding
  must be fixed or `wontfix` with a written justification before Merge.
- **Merge** — when review is closed, checks pass on the tip, and the lead is
  idle with a clean tree, the TeamManager serializes the merge. A conflict
  halts and routes the task back; it is never auto-resolved.

Tasks are independent. One task may be in Review while another is implementing
and a third is waiting on a check. Mission Control must show that reality
directly. The workflow UI is derived per task from its status, checks,
findings, and merge record; it is never a single shared step index.

## 2. Roles and tool belts

### Lead (main agent)

The lead owns the plan, the team, the review, and the merge decision. It has
the **full toolbox**:

- read/search, codegraph, memory, browser, `ask_user`;
- `create_plan` / `update_plan` / `complete_step` / `finish`;
- file tools and shell for debugging with the user;
- `git_read` (diff/log/show/status ops) and commits;
- `spawn_agent`, `message_agent`, `team_status`, `review_fix`;
- the TeamManager's merge action (auto-merge after its review).

The lead's own edits are capped by the **tiny-work rule**: either up to five
files with at most 10 changed lines per file, or one file with a larger edit.
Both paths exclude new files, schema/API/dependency/test changes, and any
cross-module refactor. The cap is enforced by an app-level external policy hook
on `file_edit` / `file_write` / `shell`; over-cap calls are denied with a
delegation nudge. Direct edits are logged as small cards in the lead chat and
counted in the cost meter. The lead may debug, run tests, read logs, and
research without a cap; the cap is about where implementation happens, not
about investigation.

Delegation is economic, not a hard lockout: do tiny/review/debug work directly;
delegate anything that needs exploration or exceeds the tiny-work cap; never
spawn a worker just to read or answer; batch related tiny fixes; choose the
cheapest capable model.

### Worker (executor)

A worker is a normal Mnemo agent running in its own git worktree, on its own
branch, with an **executor profile**:

- read/search, file tools, shell, tests, `git_read` (diff/log/show/status ops), commit;
- **no** `create_plan` / `update_plan` / `finish`;
- **no** `spawn_agent` (depth 1);
- **no** `ask_user`; it reports progress or an escalation through the
  `message_parent` host tool and finishes with a commit summary;
- a self-contained brief (plan excerpt, paths, acceptance criteria, scope
  limit) so it does not rediscover the repository;
- commits its work and reports the commit + checks it ran.

The lead's plan is the plan of record. This is the biggest cost lever: one
planning loop, not one per worker.

### Reviewer (optional)

An independent, read-only reviewer can be spawned for large or high-risk
diffs. It is a second opinion, not the gate: the lead remains the reviewer of
record. It writes a report under `.coding/reviews/` and the lead triages it.

## 3. Review protocol (lead of record)

Every worker completion goes through a lead review before any merge.

**Findings record.** The lead records findings with:
`id`, `severity` (high/low), `title`, `detail`, `file`/`line`, `status`
(`open` / `fixed` / `wontfix`), `fix_commit`, and `evidence`. The record is
shown in the worker detail and linked from the merge card.

**Lead review.** The lead inspects the diff, the changed call sites, the tests
the worker ran, and the relevant plan steps. It writes findings. It must not
merge while any finding is `open`. A `wontfix` is allowed only with a written
justification (the same rule the Mnemo reviewer protocol uses).

**Tiny findings — the lead fixes them.** A tiny finding uses the same
tiny-work cap: up to five files with at most 10 changed lines each, or one
larger single-file edit, with no new file or schema/API/dependency/test change.
The lead calls the `review_fix` host tool with a worker id and the scoped
edit(s). The TeamManager
applies the edit in the **worker's worktree**, commits it on the worker branch
with a review message, and returns the sha. The lead then closes the finding.
This is the only path that lets the lead edit a worker's branch; the lead's
own file tools remain bound to the main tree.

**Big findings — spawn a fixer.** The lead spawns a fixer subagent bound to the
same worktree/branch (or re-tasks the original worker when its context is warm
and it is within budget). The fixer gets the finding, the relevant paths, the
acceptance criteria, and the scope limit. It commits the fix and reports.

**Re-review.** The lead re-reviews the delta, not the whole branch, and closes
the finding. The merge card shows `Review: N findings · X fixed · Y wontfix`.
Any open finding blocks the merge and keeps the worker in **In review** /
**Fixing**.

The lead's review is itself a cost center. For a large diff it may spawn a
read-only reviewer to pre-read and return findings (cheaper than pulling the
whole diff into the lead's long context), then triage and fix; for small diffs
it reviews directly.

**Check binding.** The checks precondition binds to the **branch tip being merged**. After a `review_fix` commit (the TeamManager runs the affected check scope in the worker worktree) or a fixer commit (the fixer runs it as part of its report), the checks are re-run and must pass before the merge card becomes mergeable. For this repository the check scope is `cargo test --workspace`.

## 4. Auto-merge

When the review is closed and the worker's checks pass, the TeamManager merges
the worker branch into the lead's **local working branch**:

- The lead is idle and its working tree is clean; merges are serialized (one at
  a time). If the lead is busy, the merge queues.
- The merge target is never `main`. If the project is on `main`, the
  TeamManager first creates/uses a `wt/*` working branch.
- Worker branches fork from the lead's working-branch tip at dispatch. A
  base that goes stale while the worker runs is not auto-rebased; the
  conflict-halt + route-back rule is the handling.
- A conflict aborts the merge (`git merge --abort`), marks the worker
  **Conflict**, and routes it back to the worker/lead. No auto-resolution.
- The merge record stores: commit, files, `+x −y`, checks, review verdict,
  worker model/cost, and the worktree/branch. `Revert` is available before the
  branch lands.
- The worktree is removed after the merge; the branch is kept for audit.
- No push and no `main` merge. Landing the lead branch to `main` remains the
  repository's protected PR path (ruleset 23755694); the human approves it.

The agent-facing git gate still applies to the agent's own `git merge`/`push`
calls; the TeamManager's merge is an application operation with its own
policy (see `agent.md`).

## 5. Budget and cost

The team is budget-gated, not per-action-gated.

- **Caps**: max concurrent workers (a per-project setting, with no fixed
  three-worker ceiling), max spawns per lead turn (default 5), per-worker
  turns/wall-clock/cost, and a per-project budget. Worker concurrency is
  separate from budget and rate limits.
  Hitting a cap pauses the team and moves the item to **Needs you**; nothing
  runs away silently.
- **Spawn approvals**: the kernel's spawn approval is auto-answered by the
  TeamManager when the spawn fits the budget; when it does not, the approval
  stays pending and surfaces in Mission Control.
- **Model routing**: strong lead, cheap/default workers, configured reviewer;
  the lead can pick a cheaper model for mechanical work via `list_models`.
- **Model identity**: the project stores the lead model; every task stores its
  worker model. Both render as a compact badge in conversation headers, task
  rows, delegation cards, and merge records, so the human can see the model
  mix at a glance without opening telemetry.
- **Executor workers**: one plan of record (the lead's), not one per worker.
- **Self-contained briefs**: the lead attaches the plan excerpt, paths,
  acceptance criteria, and scope limit; workers do not rediscover the repo.
- **Review economics**: review small diffs directly; pre-read large diffs with
  an optional reviewer; never spawn a worker for a read or an answer.
- **Telemetry**: per-worker model, tokens, cost, elapsed, turns; a budget chip
  in the top bar; a "Pause team" / "Stop all" control; a cost line for the
  lead's direct edits.

## 6. Mission Control

Mission Control is per project, not cross-project. It is a conversation-first
task list plus an exceptions-and-budget board, not a phase dashboard or an
approval queue. Each row is one task with its own workflow and model.

States: **Dispatching**, **Working**, **In review**, **Fixing**, **Merging**,
**Merged**, **Conflict**, **Failed**, **Cancelled**.

- **Peek** (380px, over the chat side): Needs you (conflict/failed/over-budget),
  In review, Working, plus the budget chip.
- **Control room** (a route): grouped task board + conversation detail. The
  detail opens on the worker **Thread**; Review (checks, findings, merge) is a
  secondary tab. The header shows task, model, branch/worktree, and the task's
  own workflow.
- The lead is not a row; it is the chat. The team badge counts active tasks,
  not a global phase.
- The board is designed for 8+ workers: grouped, filterable, scrollable rows and
  avatar overflow instead of squeezing every worker into the top bar.
- The merge card is a record with Review/Revert, not an approval prompt.
- The lead chat shows spawn cards, team-message cards (escalations), review
  summaries, and merge records; worker tool logs stay in the worker thread.

## 7. Start screen and project lifecycle

- Recent projects (`projects.toml`) with branch, last session, and unfinished
  worktrees; open folder; create project.
- One project per instance. Closing the project returns to the start screen;
  if workers are running, confirm and stop them. Config is shared
  (`~/.mnemo`); project state is per project.
- Multiple projects = multiple Mnemo2 instances; the start screen makes this
  natural. A later "open in new window" can reuse the same engine manager.
- Second-instance WebView2 user-data-dir guard is a build item (the existing
  Mnemo app has the fix to port).

## 8. Workbench and browser

- Resizable workbench on the right, tabbed: **Browser**, **Diff**, **Plan**.
  Tabs are opened by content and remembered per project.
- Browser: native child webview on Windows, with URL/title sync, back/forward/
  reload, and an address bar; macOS falls back to a placeholder/external open
  (the existing Browser tab is Windows-only by design and gated).
- Agent browser tools run headless; screenshots land as chips in the worker
  thread. Clicking a screenshot or a chat link can open the Browser tab.
- Under ~900px the workbench becomes a full-height overlay with a back button.

## 9. Constitution and prompt

- `agent.md` now scopes the hard git merge/push gate to **Mnemo1 agent
  workflows** and adds the Mnemo2 TeamManager policy (reviewed worker branches
  only; no push; no `main`; PR landing).
- The compiled APP_RULES still say the agent's own `git merge`/`push` is
  approval-gated. That is correct for the lead's own tool calls; the TeamManager
  merge is app-level. The lead needs a small Mnemo2 prompt append explaining
  that the TeamManager performs the merge after review and that the lead must
  not call `git merge`/`push` itself. This is a build item.
- The tool-level gate (`GitTool::never_auto_for`,
  `ShellTool::never_auto_for`) remains for the agent; the TeamManager uses
  app-level git operations, not the agent's git tool.

## 10. Open decisions

- Default budget and per-project starting values (worker slots, spawns per turn,
  per-worker cost, project budget).
- Fixer default: re-task the original worker vs spawn a fresh fixer.
- Reviewer pre-pass threshold (diff size/files/risk).
- Whether workers may message the user directly or only the lead.
- Team-state persistence path (app data vs project `.coding`).
- Main landing UX (PR button vs the existing `merge_to_main` skill).

## 11. Phases

### Phase 0 — Visual prototype (complete)

A reusable Svelte 5 workspace with the start screen, lead chat, ten-worker
Mission Control, review/merge records, workbench shell, mock data, and the
conversation-first primitives. No engine, IPC, or git.

### Phase 1 — Lead engine and model registry

- Wire one real lead session to the headless engine with streaming events,
  Markdown, cancellation, and session restore.
- Load models/endpoints from configuration; add the project's `leadModel` and a
  model picker backed by the real endpoint list (`list_models`).
- Replace mock lead messages and cards with engine events while preserving the
  compact conversation contract.
- **Acceptance:** a real lead turn streams into the chat; the visible lead model
  is the configured one; no worker/team runtime is required yet.

### Phase 2 — Parallel team runtime

- Implement TeamManager: per-task records, briefs, model choice, worktrees from
  the lead branch tip, spawn limits, and independent task workflows.
- Add `message_agent`, `message_parent`, `team_status`, and `review_fix` host
  tools. Worker threads use real session events; tool logs stay in the worker
  thread.
- Parallel scheduling with configurable worker slots, budget/rate caps,
  pause/stop, and restart recovery.
- **Acceptance:** at least two tasks run concurrently, each with its own
  workflow and model; messages route both ways; no global run timeline exists.

### Phase 3 — Review, merge, recovery, cost

- Lead review records with findings; tiny-work `review_fix`; fixer routing;
  branch-tip check binding; optional read-only reviewer.
- Serialized TeamManager merge into the local lead branch, conflict
  halt/route-back, revert before landing, merge records.
- Budget enforcement, per-task telemetry, pause/stop-all, crash recovery.
- **Acceptance:** no merge with an open finding or stale check; parallel tasks
  can be in different states; recovery leaves a clean tree.

### Phase 4 — Native shell, workbench, persistence

- Tauri shell integration, native WebView2 Browser pane, diff/plan tabs,
  settings, project registry/`projects.toml`, persistence.
- Start-screen resume/cleanup, notifications, multi-instance/window handling,
  accessibility, and narrow-layout polish.
- **Acceptance:** a real project opens, sessions survive restart, the browser is
  a native pane, and no mock data path remains.

## 12. References

- `agent.md` — Mnemo1-scoped git gate + Mnemo2 team merge policy.
- `src/headless/types.rs` — `EngineOptions`, `SpawnOptions.worktree`,
  `host_tools`, `guardrails`, `SessionCommand`, `SessionEvent`.
- `src/tool/agent/spawn_agent.rs` — parent-aware spawn, approval.
- `src/runtime/mod.rs` — `ParentAwareSpawner`, `DescendantTracker`.
- `src/app/worktrees.rs` — provision/remove worktree helpers (item-shaped;
  generalize for agent ids/base ref).
- `src-tauri/src/ipc/spawn.rs` — the worktree-binding spawn pattern.
- `src/config/projects.rs` — `ProjectRegistry` / `projects.toml`.
- `src/agent/prompt.rs` — the compiled APP_RULES line (agent-facing gate).

## 13. Verification (this design lock)

- Docs/policy only: `agent.md` (the Mnemo1-scoped git gate at line 22, plus the
  new **Mnemo2 team merge policy** at lines 135-143) and this design doc. No
  `src/**` files were touched.
- No test run: no code path changed, so the workspace test gate is not
  applicable to this docs-only plan.
- `git status --short` shows only `agent.md` and the new `.coding/design/` file
  beyond the pre-existing `.coding` edits (`backlog.jsonl`,
  `plans/33a7341e.md`, and the two untracked knowledge files).
- Round-1 review (`.coding/reviews/2026-10-24-b4b6a536-mnemo2-design-lock-review.md`) raised three low findings — check binding to the merge-time tip, the worker fork base, and the `git_diff` naming. All three are closed in §2-§4 and `agent.md:139-143`.
