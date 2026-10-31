# Mnemo2 — Lead + Team (design v0.2)

Status: Phase 6 (plan board) implemented on top of Phase 5 (coexistence with
Mnemo1) — see §11 Phases 5 and 6.
Phase 4 implemented — the native host runs one real streaming lead
session and a real parallel worker team on one shared headless engine, with
per-worker branches and linked worktrees, durable reviews/findings, exact-tip
checks, serialized local merge/revert, and cumulative telemetry without token
or cost budgets.
The workbench now shows actual lead plans and worker Git deltas, a native
WebView2 Browser pane, per-project state and preferences, safe recovery, and
notifications. Phase 0b's conversation-first interface is unchanged; production
carries no mock project/worker data. This is the
design of record for the Mnemo2 frontend rebuild. It supersedes the earlier
"spaces dashboard" sketch: Mnemo2 is a single-project cockpit for a lead agent and
a team of worker subagents. The defining change in v0.2 is that the lead enforces
a workflow **per task**, tasks run in parallel, and the interface is organized
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
  acceptance criteria/scenarios, scope limit, model choice, and check scope. A
  plan that will dispatch `review_risk: "high"` carries a
  `## Acceptance scenarios` section of observable bullets before implementation.
- **Implement** — the worker edits in its worktree and commits. It may message
  the lead for a decision, but it does not replan.
- **Verify** — the check scope runs on the branch tip. A fix commit invalidates
  the previous result; the affected scope must pass again.
- **Review** — the lead performs an unanchored diff pass first, then records
  structured evidence, risk, findings, and any independent audit; every finding
  must be fixed or `wontfix` with a written justification before Merge.
- **Merge** — when review is closed, evidence is complete (and a high-risk audit
  is adjudicated at the same tip), checks pass on the tip, and the lead is idle
  with a clean tree, the TeamManager serializes the merge. A conflict halts and
  routes the task back; it is never auto-resolved.

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
- `team_dispatch`, `team_message`, `team_status`, `team_review`, `team_check`,
  `team_resolve_finding`, `review_fix`, `team_route_fix`, `team_merge`, and
  `team_revert_merge`. Generic `spawn_agent` is removed from the advertised
  registry and denied as a fallback;
- the TeamManager's app-level merge action after its review.

The lead's own edit policy is the **tiny-work rule**: either up to five files
with at most 10 changed lines per file, or one file with a larger edit. Both
paths exclude new files, schema/API/dependency/test changes, and cross-module
refactors. Phase 3 enforces the review-specific form through `review_fix`: one
existing non-test source file, a clean worker, an exact unique replacement of at
most 100 combined old/new lines, and a commit containing only that scoped edit.
Larger fixes return to the tracked worker. The broader direct-edit external
policy hook and direct-edit cost cards are not part of the landed Phase 3
surface. The lead may debug, run tests, read logs, and research without a cap.

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

Worker prompts use a dedicated Mnemo2 executor identity rather than inheriting
the original Mnemo workflow's read-only reviewer profile. File editing, shell
checks, and Git commits remain available for implementation tasks.

The lead's plan is the plan of record. This is the biggest cost lever: one
planning loop, not one per worker.

A worker can choose neither its branch nor its integration: executor gate
rules deny `git push`, `merge`, `rebase`, branch switching, and branch
deletion through both the `git` tool and the shell (Phase 5). The deny is a
veto, so it holds under the workers' `OverridePermissions`.

### Audit reviewer (high risk)

An independent, read-only **audit worker** is required for `review_risk: "high"`
work — shared contracts, persistence, concurrency, security, and large or
cross-cutting changes — and remains optional for standard work. It is a fresh
context, not a second opinion the lead can ignore: the audit is a merge
precondition that the lead must adjudicate. The lead remains the reviewer of
record.

`team_audit_dispatch { worker_id, reviewer_id, model?, reasoning_effort?, brief? }`
creates the audit worker against the implementation worker's current branch/tip
and plan context. It receives the plan/intent and target diff; it does **not**
receive the implementation worker's review summary, findings, or conclusions.
The audit worker has no plan step, and is read-only by construction: no file
mutation, no commits, no merge, no `spawn_agent`, no `ask_user`, no plan ticking.
Its completion notice asks the lead to adjudicate the target review, never to
merge the reviewer. A different model is preferred when available, but fresh
context is the independence requirement — a different model is not mandatory.

## 3. Review protocol (lead of record)

Every worker completion goes through a lead review before any merge.

**Findings record.** The lead records findings with:
`id`, `severity` (high/low), `title`, `detail`, `file`/`line`, `status`
(`open` / `fixed` / `wontfix`), `fix_commit`, and `evidence`. The record is
shown in the worker detail and linked from the merge card.

**Unanchored pass, then evidence.** The lead reviews in two phases. First it
reads the target diff *unanchored* — `git_read` op `diff`/`show`, the Diff tab, or
`git diff <base>..<tip>` — and forms its own opinion **before** reading the
worker's review summary, findings, or test claims. Only then does it verify the
worker's conclusions against the diff and record one evidence object per review:

```text
evidence: {
  unanchored_pass: boolean,
  intent: string,
  execution: string,
  integration: string,
  test_quality: string,
  residual_risk: string,
  independent_reviewer_id?: string | null,
  audit_adjudication?: string | null
}
```

- `unanchored_pass` — true only when the raw-diff pass happened for the exact
  tip under review.
- `intent` — what the change was meant to do and whether the diff matches the
  plan/acceptance scenarios; quote the diff, not the brief.
- `execution` — the real execution path: changed call sites, error handling, and
  what can now run differently.
- `integration` — interactions with existing contracts, persistence,
  concurrency, and neighbouring modules; who calls what changed.
- `test_quality` — whether the tests genuinely exercise the change, including a
  red-before/green-after regression test for a defect fix; a passing count alone
  is insufficient.
- `residual_risk` — what remains unverified, out of scope, or reliant on an
  assumption.

A **standard** review requires `unanchored_pass: true` and all five assessment
strings nonempty; a **high-risk** review additionally requires
`independent_reviewer_id` and `audit_adjudication`. No findings is a recorded
conclusion, not an absent one: the lead writes a substantive rationale naming
what it inspected and why the delta holds. "Looks good", a restated worker
summary, or a test count does not satisfy the gate. It must not merge while any
finding is `open`; a `wontfix` requires a written justification (the same rule
the Mnemo reviewer protocol uses).

**Risk and independent audit.** `team_dispatch` accepts
`review_risk: "standard" | "high"` (default standard); the lead marks shared
contracts, persistence, concurrency, security, and large or cross-cutting work
**high**, because those are where implementation context most biases judgment. A
high-risk dispatch requires the plan's `## Acceptance scenarios` section with at
least one observable bullet, written before implementation. A high-risk worker
cannot merge on lead review alone: the lead runs `team_audit_dispatch` (see
§2), links the completed audit as `independent_reviewer_id`, and records what it
accepted or rejected in `audit_adjudication`. At review and merge the host
validates the linked worker is `kind: "audit"`, targets the same implementation
worker, inspected the exact tip, and reached a terminal non-failed state. Worker
records carry `kind` (default `"implementation"`), `review_risk` (default
`"standard"`), and optional `audit_target`/`audit_tip`; legacy records
deserialize as implementation/standard with no evidence and need a fresh review
(plus an audit where high risk) before merge.

**Tiny findings — the lead fixes them.** Phase 3 constrains this to one
existing non-test source file and an exact unique replacement of at most 100
combined old/new lines, with no new file or schema/API/dependency/test change.
The lead calls `review_fix` with the worker id, finding id, and scoped strings.
The worker checkout must be clean. TeamManager applies the edit in the worker's
worktree, commits only that file on the worker branch, stores the sha and
evidence, and invalidates the prior review/checks. This is the only path that
lets the lead edit a worker's branch; the lead's own file tools remain bound to
the main tree.

**Big findings — route to the tracked worker.** Phase 3 re-tasks the original
worker on the same worktree/branch. The worker gets the finding, relevant path,
acceptance criteria, and scope limit, then commits and reports. A fresh fixer
subagent remains an optional later refinement.

**Re-review.** The lead re-reviews the delta, not the whole branch, and closes
each finding with fixed evidence or a written `wontfix` justification. The merge
card shows `Review: N findings · X fixed · Y wontfix`. Any open or unevidenced
finding blocks merge and keeps the worker in **In review** / **Fixing**. A fix
commit invalidates the prior lead review: the lead repeats the unanchored pass,
replaces the evidence for the new tip, and a high-risk worker needs a **fresh
audit** — the earlier `audit_tip` is stale and its adjudication no longer
applies.

The lead's review is itself a cost center. It reviews small diffs directly; a
large or high-risk diff gets the independent audit above rather than an
unreviewed pre-read.

- The lead and worker are idle and both working trees are clean. Review,
  checks, dispatch, messaging, stop/resume, and merge/revert share one operation
  lock, so exact-tip evidence cannot change after validation. A busy lead or
  worker is rejected with an actionable error rather than silently queued.
- The gate requires complete review evidence for the current tip, and for
  `review_risk: "high"` a linked completed audit whose `audit_tip` is that tip
  plus a nonempty lead adjudication. An open/unevidenced finding, incomplete
  evidence, or a stale audit keeps the worker in review.
- The merge target is never `main`. If the project is on `main`, merge refuses
  until a `wt/*` working branch exists.
- Worker branches fork from the lead's working-branch tip at dispatch. A
  base that goes stale while the worker runs is not auto-rebased; the
  conflict-halt + route-back rule is the handling.
- A conflict aborts the merge (`git merge --abort`), marks the worker
  **Conflict**, and routes it back to the worker/lead. No auto-resolution.
- The merge record stores: commit, files, `+x −y`, checks, review verdict with
  its structured evidence, `review_risk`, any linked audit, worker model,
  cumulative tokens, and pricing-derived cost when available.
  `Revert` requires the original non-`main` lead branch, the merge in current
  history, and no evidence that it already landed on `main`.
- The worker session is closed before its worktree is removed; the branch is
  kept for audit. A merged worker cannot be resumed.
- Recovery aborts an in-progress conflict and verifies a clean lead. A merge
  already committed before a crash is reconstructed only when the lead commit's
  second parent proves the reviewed worker tip; ambiguous cases route back.
- No push and no `main` merge. Landing the lead branch to `main` remains the
  repository's protected PR path (ruleset 23755694); the human approves it.

The agent-facing git gate still applies to the agent's own `git merge`/`push`
calls; the TeamManager's merge is an application operation with its own
policy (see `agent.md`).

## 5. Session limits and cost

The team has operational session limits, not per-agent spending budgets.

- **Operational limits**: `DEFAULT_SLOTS = 3` live worker sessions. There is no
  per-lead-turn delegation limit (the four-per-turn cap was removed by user
  decision 2026-10-31); the concurrent slot limit is the only spawn limit.
  Worker sessions have no turn, wall-clock, token, or cost limit
  (`limits: None`); the lead or the user stops a worker explicitly. There is no
  cumulative worker token or cost budget and no budget-increase flow. Prompt,
  completion, cached, and context-window usage never pause a worker or block a
  message/resume. Provider context limits and parked sessions remain actual
  lifecycle outcomes, not spending limits.
- **Tracked, idempotent dispatch**: the lead chooses a stable worker ID for each
  logical task and retains the returned JSON receipt and authoritative status.
  Reusing that ID with the same brief/model returns the existing worker rather
  than consuming another spawn or creating another session/worktree; a changed
  brief/model conflicts. An unknown explicit model is rejected before any
  record, branch, or worktree is created. Generic spawning is unavailable.
- **API authority**: TeamManager records and `team_status` are the only worker
  lifecycle/ID authority. The lead never infers lifecycle from Git/files or
  edits `team.json`; TeamManager owns its ledger and Git operations internally.
- **Model routing**: strong lead, worker model and reasoning effort chosen per
  task. `team_dispatch` advertises the configured `endpoint/model` specs and
  the effort levels as schema enums; omitting them uses the configured
  defaults. The UI picker shows endpoint, model, and effort unambiguously.
  From Phase 5 the lead routes on one `lead` slot instead of per workflow
  state, so its model never changes silently between Planning and Executing.
- **Model identity**: the project stores the lead model; every task stores its
  worker model. The composer/settings show the lead model and compact badges
  identify worker models in task rows, delegation cards, and merge records.
- **Executor workers**: one plan of record (the lead's), not one per worker.
- **Self-contained briefs**: the lead attaches the plan excerpt, paths,
  acceptance criteria, and scope limit; workers do not rediscover the repo.
- **Review economics**: review small diffs directly; a large or high-risk diff
  gets the required independent audit rather than an unreviewed pre-read; never
  spawn a worker for a read or an answer.
- **Blocked workers**: worker messages and blocked/failure transitions send a
  deduplicated, attributed notice to the actual lead session. The lead uses
  `team_message`, `team_stop`, `team_resume`, and `team_compact` with exact
  returned IDs. Compaction is a real session command whose requested and actual
  completed/failed states remain distinct; it never creates a duplicate session.
- **Telemetry**: per-worker model, cumulative prompt/completion/cached tokens,
  requests, elapsed time, and completed turns, all persisted across sessions.
  Cost is derived only from configured model pricing; unknown cost is never
  shown as zero. The aggregate chip reports actual usage/cost without a token
  denominator. Workers support pause/resume/stop and real context compaction.
- **Deletion**: the user can delete one or more workers with their record,
  transcript, checkout, and branch (a branch another worker still uses is
  kept). A live session is stopped first. The confirmation sheet names what
  each delete removes and warns about unmerged commits or uncommitted changes
  the lead branch lacks.
- **Needs you**: every Needs-you banner and row carries the actual question,
  approval, or blocking reason, never a bare "needs your attention".

## 6. Mission Control

Mission Control is per project, not cross-project. It is a conversation-first
task list plus an exceptions-and-usage board, not a phase dashboard or an
approval queue. Each row is one task with its own workflow and model.

States: **Dispatching**, **Working**, **Needs you**, **Idle**, **Paused**,
**Interrupted**, **Completed**, **Stopped**, **Failed**, **In review**, **Fixing**,
**Merging**, **Merged**, **Conflict**, **Cancelled**.

- **Peek** (380px, over the chat side): Needs you (conflict/failed/blocked),
  In review, Working, plus the actual usage/cost chip.
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
- The lead chat also shows the lead's own real tool calls as compact,
  human-readable activity rows, with dotted connectors across consecutive
  calls. A native disclosure reveals the raw tool name, formatted arguments,
  bounded live tail, and final result or error; failed calls retain a short
  collapsed summary. Live `SessionEvent` call starts/deltas/results and
  persisted transcript tool calls both feed the same reducer; a host-tool call
  renders as team work in progress, not as an approval decision, and no tool
  activity is synthesized. Tool rows use observed start/result times for real
  elapsed durations, not a placeholder `now`. While the lead is busy and no
  tool is running, an inline Thinking indicator with a live turn timer occupies
  the next conversation position; the next tool call replaces it. Stop remains
  available while the lead is busy or waiting for input, and reports interruption
  failures without claiming an unconfirmed stop. Provider errors, including
  automatic retries, stay visible at the conversation tail across same-session
  snapshots; terminal errors offer Retry and later progress clears retry warnings.
- Human messages remain left-aligned and inline in actual exchange order.
  The transcript uses the available width, caps prose at 100 characters while
  leaving code/tool details unconstrained, and avoids oversized card spacing.
- Markdown renders sanitized CommonMark/GFM, including task-list checkboxes,
  tables, strikethrough, and autolinks. It preserves literal punctuation,
  styles every heading level plus quotes/rules, and routes Plan links into the
  Browser pane. The last committed frame stays while the newest source renders
  on one coalesced animation frame, so streaming never flashes an empty block.
  The thread follows new content only while the user is within 48px of the
  bottom; a user scroll-up is respected, and a `ResizeObserver` follows
  asynchronous Markdown/content growth while sticky.

## 7. Start screen and project lifecycle

- Recent projects (`projects.toml`) with branch, last session, and unfinished
  worktrees; open folder; create project.
- One project per instance. Closing the project returns to the start screen;
  if workers are running, confirm and stop them. Config is shared
  (`~/.mnemo`); project state is per project.
- Multiple projects = multiple Mnemo2 instances; the start screen makes this
  natural. A later "open in new window" can reuse the same engine manager.
- Per-instance WebView2 profiles and same-project markers are shared with
  Mnemo1. A second live instance on the same project requires an explicit
  confirmation; cleanup refuses active projects and preserves newer markers.

## 8. Workbench and browser

- Resizable workbench on the right, tabbed: **Browser**, **Diff**, **Plan**.
  Tabs are opened by content and remembered per project.
- Browser: native child webview on Windows, with URL/title sync, back/forward/
  reload, and an address bar. Other platforms offer external open in the system
  browser; no embedded pane is simulated.
- Agent browser tools run headless; screenshots land as chips in the worker
  thread. Clicking a screenshot or a chat link can open the Browser tab.
- Under ~900px the workbench becomes a full-height overlay with a back button.

## 9. Constitution and prompt

- `agent.md` now scopes the hard git merge/push gate to **Mnemo1 agent
  workflows** and adds the Mnemo2 TeamManager policy (reviewed worker branches
  only; no push; no `main`; PR landing).
- The compiled APP_RULES still say the agent's own `git merge`/`push` is
  approval-gated. That is correct for the lead's own tool calls; TeamManager's
  merge is app-level. `WorkflowPolicy::stable_head` now appends
  `MNEMO2_TEAM_RULES` when generic spawning is blocked, explaining tracked
  delegation, exact-tip review/checks, tiny-fix scope, and why the lead must not
  call `git merge` or `git push` itself.
- Phase 5 replaces the classic lifecycle map and closing sequence in the lead's
  head with team versions (§11 Phase 5.1) and scopes `agent.md` sections by
  workflow mode (Phase 5.2), so neither the lead nor a worker reads rules for
  tools it does not have.
- The tool-level gate (`GitTool::never_auto_for`,
  `ShellTool::never_auto_for`) remains for the agent; the TeamManager uses
  app-level git operations, not the agent's git tool.

## 10. Open decisions

- Custom worker-slot defaults are persisted.
- Whether a fresh fixer subagent is ever preferable to the current original-worker
  re-tasking path.
- Whether workers may message the user directly or only the lead.
- Main landing UX (PR button vs the existing `merge_to_main` skill).
- A read-only reviewer pre-pass for large diffs, as a TeamManager-tracked worker
  with a reviewer profile (it would add its report to the plan's team review).
- Seeding each worker worktree's code graph from the lead's
  `.coding/codegraph.db` instead of letting it index from scratch.
- Whether Mnemo1's runall and Mnemo2's team should ever share one landing
  helper (today runall lands `wt/runall-*` on `main` or a PR; TeamManager merges
  `wt/agent-*` into the lead branch).

## 11. Phases

### Phase 0 — Visual prototype (complete)

A reusable Svelte 5 workspace with the start screen, lead chat, ten-worker
Mission Control, review/merge records, workbench shell, mock data, and the
conversation-first primitives. No engine, IPC, or git.

### Phase 1 — Lead engine and model registry (complete)

Shipped in the native host:

- **Separate shell.** `src-tauri/mnemo2-app/tauri.conf.json`, `src-tauri/src/bin/mnemo2.rs`,
  and `src-tauri/src/mnemo2/` add a minimal Tauri host without extending Mnemo1's
  IPC surface. The host runs on Vite port 5180 and embeds
  `target/mnemo2-dist`. The host is gated by the `mnemo2` Cargo feature so the
  Mnemo1 main binary never links its Tauri context.
- **Lead runtime.** `LeadRuntime` starts the headless `Engine`, registers
  `WorkflowPolicyFactory` as `mnemo.workflow`, and spawns one lead session under
  `ApproveViaApi` / `ApproveEachAction`. It forwards `SessionEvent`s, persists
  the transcript and selected `endpoint/model` spec, restores them on reopen,
  interrupts a running generation, and changes models only while idle.
- **Model registry.** The picker loads the configured endpoint/model specs from
  `Config::load` (the same `endpoints.toml` inventory), not a network probe. The
  UI shows the actual serving model id reported by the session and keeps the
  selected spec separate for resume.
- **IPC and events.** Commands: `lead_open`, `lead_snapshot`, `lead_prompt`,
  `lead_interrupt`, `lead_set_model`, `lead_approve`, `lead_answer`,
  `lead_close`. `lead_prompt` takes `{ text, images }` — base64 data-URL
  attachments ride the turn exactly like the images of a headless
  `SessionCommand::Prompt`, and the `images` key may be omitted by legacy
  callers. A busy lead receives the submission as a queued steer
  (`SessionCommand::Steer`, same payload) injected at the next safe turn
  boundary instead of an error. One event channel, `mnemo2://lead-event`, carries
  `{ generation, event }`; a model switch advances the generation and stale
  events are ignored. App-owned state lives under the Tauri app-data directory
  (`lead/lead.json`, `lead/workspace/`), never a tracked project file.
- **UI.** The lead composer sends through the real session in native mode and
  falls back to the Phase 0b mock reply in browser-only Vite. It exposes a
  compact real-model picker (composer and project settings), a stop button while
  working, inline error/retry, and inline approval/question cards. Workers, team
  workflows, checks, diffs, and merge records remain mock data.
- **Acceptance:** a real lead turn streams into the chat; the visible lead model
  is the configured serving model; cancellation, restore, and model switching
  are exercised by the runtime and store tests; no worker/team runtime is
  required yet. Worker/team views remain unchanged.
- **Known limitation:** native open still uses the Phase 0b seeded project paths.
  A seed that does not exist on the current machine surfaces the runtime's inline
  "project root does not exist" error; the native project registry/folder picker
  is Phase 4. The browser fallback remains filesystem-free.

### Phase 2 — Parallel team runtime (implemented)

- **Landed:** `TeamRuntime` (`src-tauri/src/mnemo2/team.rs`) spawns workers on the
  lead's shared `Arc<Engine>`. Each worker gets a durable record (brief, model
  spec, observed model id, branch, worktree, status, generation, transcript,
  pending interaction, token usage, checks) persisted to `team.json`.
- **Worktrees:** `src/app/worktrees.rs` now has `agent_branch` /
  `provision_agent_worktree` / `remove_agent_worktree` alongside the runall
  helpers. Workers fork from the lead's current branch tip (never `main`) onto
  `wt/agent-<id8>` at `<project>/.worktrees/agent-<id8>`; `-b` makes a duplicate
  branch fail loudly. `Engine::spawn` maps `SpawnOptions.worktree` into a
  `WorktreeBinding`, so the worker's sandbox, project root, plans dir, and code
  graph all resolve inside its own checkout.
- **Executor surface:** `ToolSelection::all()` minus `create_plan`,
  `update_plan`, `finish`, `spawn_agent`, and `ask_user`. The original turn and
  wall-clock limits were later removed (§5). Planning and review stay the
  lead's job.
- **Messaging:** the lead gets `team_dispatch` / `team_message` / `team_status`
  host tools; workers get `message_parent`, whose messages land in a lead inbox
  and render in the lead conversation. A worker's direct `spawn_agent` is denied
  by a `WorkflowPolicy::with_blocked_tools` veto, so every worker must be tracked
  by the team runtime. (The plan sketched `message_agent`/`review_fix`; the
  landed lead-side tools are `team_message` and `team_status`, with
  `review_fix` deferred to Phase 3 along with review itself.)
- **Scheduling:** configurable live-session slots (`DEFAULT_SLOTS = 3`) are
  the only spawn limit — there is no per-lead-turn delegation cap (removed
  by user decision 2026-10-31). Pause, interrupt, stop, and resume drive
  real `SessionCommand`s.
- **Recovery:** on open, `dispatching`/`working`/`needs_you` records downgrade to
  `interrupted` (or stay `needs_you` when a pending interaction survived);
  `team_resume` reattaches the saved transcript against the worker's worktree.
- **Projects:** the project registry/`projects.toml` and folder picker landed
  early (pulled out of Phase 4) so no seeded path survives.
- **Acceptance met:** two workers run concurrently with independent models,
  statuses, branches, worktrees, and transcripts, and no event cross-talk; the
  lead and workers exchange messages both ways; no global run timeline exists.
  At the Phase 2 boundary, review/merge controls were disabled rather than
  simulated; the placeholders are removed in Phase 3.

### Phase 3 — Review, merge, recovery, cost (implemented)

- **Review and fixes:** durable lead review summaries/findings and exact-tip
  checks, fixed evidence and written `wontfix`, constrained one-file
  `review_fix` over canonical, symlink-free source paths, and larger-finding
  routing back to the tracked worker. Every fix invalidates stale review/check
  evidence and requires delta review.
- **Integration:** one operation lock covers dispatch, review/check/fix,
  messaging, stop/resume/compact, and merge/revert. Merge requires an idle clean
  lead and worker, a non-`main` target, exact-tip passing checks, and closed
  findings. Verified conflict-only merges abort with diagnostics and route back;
  unrelated lead changes block automatic recovery and are preserved. Guarded
  revert works only before landing. Merge records include diff stats, model,
  tokens, and cost when priced. Sessions close before verified-clean worktree
  cleanup; branches and dirty work remain for audit.
- **Lifecycle and recovery:** caller-stable dispatch IDs, structured worker
  receipts, model validation before provisioning, real usage/turn/activity/
  elapsed telemetry without token or cost budgets, pricing-derived cost,
  stop-all, blocked-worker lead notices, real context compaction, and restart
  downgrade. `team.json` uses a backup-recoverable replacement path. Dispatch
  rollback preserves advanced/dirty work; committed merges are reconstructed
  only from verifiable Git parent evidence.
- **Native/UI:** typed review/fix/check/merge/revert/stop/resume/compact commands
  and real worker/inline lead cards. Generic `spawn_agent` is hidden, Markdown is
  retained across streaming, and no placeholder success path remains.
- **Acceptance met:** no merge with an open/unevidenced finding or stale check;
  parallel tasks keep independent states; exact-tip evidence cannot race the
  operation lock; conflict-only recovery leaves a clean tree while unrelated
  changes survive; committed crash recovery never invents a merge result; and
  retrying a logical dispatch cannot duplicate a worker.

### Phase 4 — Native shell, workbench, persistence (implemented)

- **Real workbench:** `AgentSession::plan()` reads the active plan from that
  session's own plans directory. `workbench_diff` reads the selected worker's
  actual Git base/tip and checkout, including untracked files; merged workers
  use the merge's first parent and reviewed worker tip. Cleaned branches stay
  reviewable without a checkout. Binary, sensitive, oversized, and truncated
  content is labelled rather than silently completed.
- **Native Browser:** a child WebView2 on Windows with real history, reload,
  address navigation, URL/title events, physical-pixel resize, and overlay
  hiding. No iframe or CDP path exists. Other platforms offer external open.
  Worker screenshots are read only from the sandbox screenshot directory and
  shown as actual image previews; chat links open the same Browser pane.
- **Per-project persistence:** `<project>/.coding/mnemo2/` (gitignored, with
  an owner stamp naming the canonical project) holds `lead.json`, `team.json`,
  `preferences.json`, and the engine workspace. It replaced the original
  app-data `projects/<sha256>/` location; the old app-data history was wiped
  rather than migrated. Lead transcripts carry project identity and save time.
  Worker slots and workbench preferences survive restart, with backup recovery.
- **Lifecycle:** real Git/session metadata, new-project creation, Resume /
  Review / Clean, same-instance warnings, close confirmation, and conservative
  cleanup of only inactive clean worktrees. Every branch is retained;
  cancelled workers are review-only. The official notification plugin reports
  deduplicated worker completion, input requests, failures, and conflicts when
  the app is unfocused.
- **Shell and accessibility:** the bare window is created after installing the
  per-instance profile. Its child browser respects tabs, dialogs, drag resize,
  and narrow layouts. Library tests link the Tauri COMCTL32 v6 manifest, fixing
  the `TaskDialogIndirect` startup regression. Modal focus management and
  narrow-layout spacing keep the native pane from covering controls.
- **Acceptance:** a real project opens, sessions survive restart, the browser is
  a native pane, and no mock data path remains.

### Phase 5 — Coexistence with Mnemo1 (implemented)

Mnemo1 and Mnemo2 share one engine, one prompt head, and one set of plan
tools. Through Phase 4 the Mnemo2 lead ran Mnemo1's classic workflow
unchanged with a team-rules block appended. Several classic rules contradict
the team model, and three of them break it:

| Collision | Effect before Phase 5 |
|---|---|
| `finish` needs a reviewer-written `.coding/reviews/` report. Only a `spawn_agent` reviewer can write one, and Mnemo2 blocks `spawn_agent`. | An implementation plan can never leave Reviewing; only `abandon_plan` escapes. |
| Executing/Reviewing turns auto-continue up to 12 times. Team workers are not tracked descendants, and a finished worker turn sends no notice. | The lead polls or does the worker's task itself instead of idling. |
| Workers run under `OverridePermissions` with no gate rules. | A worker's `git push` or `git merge` runs with only an audit event. |
| The engine starts with memory disabled and no code graph, while the head marks memory and graph tools MANDATORY. | Dead mandates. Mnemo2 neither reads nor feeds the project memory. |
| APP_RULES say "the main agent can NEVER author a review"; Mnemo2 makes the lead the reviewer of record. | Contradictory authority and two review record systems. |
| Team host tools are AutoRun, so the Planning/Complete filters admit them. | Dispatch and merge without a plan. |
| `complete_step` knows nothing about tasks. | Steps tick before their work merges. |
| Lead plans live in the gitignored `.coding/mnemo2/workspace`. | Invisible to Mnemo1; the plan file stops being the crash-resumption document. |
| Workers inherit all of `agent.md`. | Mnemo1-only process rules for tools workers lack. |
| The lead routes its model per workflow state. | With no pinned model, the lead's model can change between states. |

**Principle: one engine, two workflow modes.** The workflow is a policy
choice: `WorkflowMode::Classic` for Mnemo1 and `WorkflowMode::TeamLead` for the
Mnemo2 lead. Executor workers keep their declarative guardrails. The universal
rules stay shared and byte-identical for Mnemo1: tool-call discipline, file
tools, never commit to `main`, the approval gate on the lead's own git core
operations, and memory hygiene. Only the workflow-specific text and gates
differ.

#### 5.1 TeamLead workflow mode (engine)

> Superseded in part by Phase 6. The single-active-plan state gate, the
> `complete_step` gate, and the ledger-backed `finish` gave way to the plan
> board. The team head, turn-end Stop, `lead` routing, and the ledger seam
> remain.

- **Construction.** `WorkflowPolicyFactory::team_lead(ledger, plan_gated_tools)`
  replaces `with_blocked_tools(["spawn_agent"])`. It builds `WorkflowPolicy` in
  `TeamLead` mode, with `spawn_agent` blocked and a `TeamLedger` handle. That
  trait lives in `src/app/team_ledger.rs`; the Mnemo2 host implements it over
  `TeamRuntime`. Classic behavior stays byte-identical, pinned by the existing
  head and tail tests.
- **Stable head.** TeamLead swaps two blocks:
  - `TEAM_WORKFLOW_LIFECYCLE` replaces the lifecycle map.
  - `TEAM_APP_RULES` keeps the universal rules but replaces the closing
    sequence: every task stamped with the plan is merged or cancelled, the
    checks pass on the lead tip, the lead commits its own changes, then
    `finish`.

  It also drops the `spawn_agent` strategy line and keeps
  `MNEMO2_TEAM_RULES`.
- **Volatile tail.**
  - EXECUTING: dispatch the current step's tasks, end the turn, act on
    TeamManager notices, review and merge, then `complete_step`.
  - REVIEWING: confirm with `team_status` that every plan task is merged or
    cancelled, run the checks, commit, then `finish` — TeamManager evidence is
    the review gate.
- **Turn end.** `on_turn_end` returns Stop while the ledger reports a live
  worker turn (Dispatching, Working, Fixing, or Merging); TeamManager notices
  wake the lead. With no live worker turn the classic Continue rule applies,
  so a lead mid-step with no delegated work still makes progress.
- **State gate.** In Planning and Complete, `gate_call` denies the mutating team
  tools: `team_dispatch`, `review_fix`, `team_route_fix`, `team_merge`, and
  `team_revert_merge`. The host passes this list. Observing and managing
  workers stays available in every state, so the lead can still handle a
  straggler after a plan closes.
- **Step gate.** `complete_step` is denied while the ledger reports an open task
  for that plan and step. A task is open until it is merged or cancelled; the
  denial names the open tasks.
- **Finish.** In TeamLead mode `review_report` is optional.
  - Without it, `finish` asks the ledger for the plan's team review. Any
    unmerged, uncancelled task or open finding blocks finish.
  - Otherwise the ledger returns a Markdown report opening `## Verdict: PASS`:
    tasks, worker models, merge commits, findings with evidence or `wontfix`
    justification, and checks.
  - `finish` writes that report to
    `.coding/reviews/team-<plan8>-<timestamp>.md`, then runs the normal gates:
    the bug regression test, landed design, and the PLAN:/BUG: capture.
  - An explicit `review_report` still works.

  Mnemo1 and Mnemo2 reports share one record system in `.coding/reviews/`.
- **Routing.** TeamLead routes the lead on a single `lead` slot that falls back
  to the default model, not a per-state slot. A pinned model still wins.

#### 5.2 Host wiring (Mnemo2)

- **Executor gate rules.** `executor_guardrails` gains Deny rules for the `git`
  tool and `shell`: push, merge, rebase, branch switch (`switch`, and a
  `checkout` of a branch rather than of paths), branch deletion, worktree
  commands, and `gh pr`. Each message says that TeamManager owns integration. A
  deny is a veto, so it holds under `OverridePermissions`.
- **Completion notice.** A worker turn that ends Completed sends the lead a
  deduplicated TeamManager notice ("finished its turn — review it"), like the
  existing blocked, failed, and message notices. Together with the turn-end
  Stop, this is what lets the lead idle.
- **Plan stamping.** `team_dispatch` records the lead's active plan id and
  current step index on the worker record. The host reads them from the lead
  session's plan, never from model arguments. A dispatch outside a plan
  records none and is refused by the state gate anyway.
- **Ledger.** `TeamRuntime` implements `TeamLedger`: live worker turns, open
  tasks per plan and step, and the plan's team review report.
- **Shared project knowledge.** The Mnemo2 engine uses the project's own
  `.coding/memory.db`, `.coding/knowledge/`, and `.coding/codegraph.db` — the
  same stores Mnemo1 uses — so both apps learn from and feed one memory. The
  policy factory gets the memory, knowledge, code graph, and `.coding/reviews/`
  handles, so `create_plan` recall and the `finish` capture work. Workers share
  the memory store; each worktree's code graph keeps Phase 2's binding.
- **Lead plans.** The lead's plans live in `<project>/.coding/plans/`, the same
  stack Mnemo1 uses, and stay stable across lead respawns and model switches.
  A plan started in either app is visible in the other, and the plan file is
  again the git-tracked crash-resumption document. The merge's clean-lead check
  ignores `.coding/plans/` and `.coding/knowledge/` changes; the lead commits
  them with its own work.
  Concurrent use of one project by both apps stays behind the existing shared
  same-project marker and its explicit confirmation.
- **Constitution scope.** `agent.md` wraps Mnemo1-only process rules in
  `<!-- mnemo:classic -->` … `<!-- /mnemo:classic -->` and Mnemo2-only rules
  in `<!-- mnemo:team -->` … `<!-- /mnemo:team -->`. The loader drops team
  sections for Classic sessions and classic sections for the TeamLead and
  executor workers. Unmarked text applies to everyone.
- **Worktree isolation.** Each app provisions and cleans only its own
  `.worktrees/` prefix (`runall-*` for Mnemo1, `agent-*` for Mnemo2), and tests
  pin this.

#### 5.3 Rollout

1. Safety and liveness: executor gate rules and the completion notice.
2. TeamLead mode in the engine (5.1), with unit tests; Classic stays
   byte-identical.
3. Host wiring (5.2): ledger, plan stamping, shared knowledge, lead plans
   directory, constitution scope, and worktree isolation.
4. Verification: `cargo test --workspace`, the `mnemo-app` mnemo2 tests,
   `npm --prefix mnemo2 test`, and this document's results.

#### 5.4 Acceptance

- A Mnemo2 implementation plan reaches Complete through `finish` with no
  `spawn_agent`. The generated report lands in `.coding/reviews/`, and the
  PLAN digest lands in the project memory Mnemo1 reads.
- After dispatching, the lead ends its turn and the completion notice wakes it;
  no synthetic continue fires while a worker turn runs.
- A worker's `git push` or `git merge`, through the git tool or the shell, is
  refused.
- `team_dispatch` in Planning or Complete is refused, and `complete_step` with
  an open task is refused.
- Mnemo1's Classic head and tail bytes are unchanged.

#### 5.5 Landed decisions and verification

- **Stopped counts as closed.** The lead's `team_stop` ends in Stopped; only
  the user's workspace cleanup reaches Cancelled. The plan gates therefore
  treat Merged, Cancelled, and Stopped as closed. Stopped and
  merged-then-reverted tasks are listed under "Not merged" in the team review
  report, and findings on abandoned tasks do not block.
- **Shared stores through the state dir.** The Mnemo2 engine workspace is now
  `Workspace::new(root)`, so its state dir is `<project>/.coding`. Memory,
  knowledge, code graph, plans, and reviews all derive from it, which keeps the
  memory reconcile and the plan stack consistent. The code graph follows
  `general.codegraph` as in Mnemo1. The engine also reads the project's
  `.coding/safety.toml`, `mcp.toml`, and `logs/`. `start_engine` adds Mnemo1's
  managed ignore entries to `.git/info/exclude` so database files never dirty
  the lead checkout. New engine accessors: `memory_store_handle`,
  `knowledge_store_handle`, `codegraph_handle`, `reviews_dir`.
- **Lead plans.** `Engine::use_project_plans()` (default off) makes
  worktree-less spawns use the project's `.coding/plans/`. `SpawnOptions` is
  unchanged.
- **Executor gate rules.** The `git` tool has no rebase, switch, or worktree
  operations, so its rules cover push, merge, checkout, and branch deletion.
  Shell rules cover the full list, including chained, `-C`, and `bash -c`
  forms. They match only the `command` field.
- **Constitution.** In this repository's `agent.md`, classic-only sections are
  Documentation expectations, Review expectations, the `merge_to_main`, landing
  bookkeeping, create_plan auto-fork, and run-all bullets, and the "before
  marking a workflow step complete" test rule. The Mnemo2 team merge policy is
  team-only. Workers run the checks named in their brief.
- **Verification.** `cargo test --workspace`: 3544 passed, 0 failed. This
  includes the prompt goldens, which show the Classic head and tail unchanged,
  and the boundary test (`GateVerdict` is now re-exported from
  `mnemo::headless`). `cargo test -p mnemo-app --features mnemo2 --lib mnemo2`:
  88 passed. `cargo build -p mnemo-app --features mnemo2 --bin mnemo2`: passed.
  `npm --prefix mnemo2 test`: 156 passed. No live-provider run has exercised a
  full plan through `finish` yet.

### Phase 6 — Plan board (implemented)

Phase 5 kept the lead on Mnemo1's one-active-plan state machine. That turned
out stricter than intended: the lead could not run several plans at once, and
every dispatch, step, and `finish` hung off a single active plan. Phase 6
keeps "plan first" but lets the lead hold any number of plans and run them in
parallel through its workers.

**Why plans matter here.** A plan is the record of intent: why the work
exists, its constraints, the decisions taken along the way, and how each step
turned out. Code does not carry any of that. Plans are therefore mandatory for
all delegated work, written to `.coding/plans/` in Mnemo1's plan-file format,
kept after they close, and digested into project memory.

#### 6.1 Rules

- **Plan first.** The lead plans before it delegates. `team_dispatch` requires
  a `plan_id` and a 1-based `step`, and refuses an unknown plan, a closed plan,
  a missing step, or a step that is already done. The lead's head tells it to
  plan all work first, but the lead's own small direct edits are not gated.
- **Many plans.** The lead creates any number of plans; each is open until it
  closes. No plan is "active", and no workflow state locks the lead's tools.
- **Workers get the plan.** TeamManager builds every brief from the plan file:
  the plan's title, goal, kind, bug symptom, and context, the full text of the
  assigned step, a step list for orientation, and the line "You are working on
  step N of plan <title> (<id>); other steps are handled separately". The
  lead's brief adds only the task-specific instructions. Several tasks may work
  on one step.
- **Steps tick on merge.** A step is checked off in the plan file once at least
  one of its tasks merged and none is still open. Reverting the last merged
  task for a step unticks it. A step with no worker task (lead-only work) is
  ticked by the lead with `plan_step_done`, which is refused while that step
  has open tasks.
- **Outcomes are recorded.** Each tick appends one outcome line to the plan's
  `## Progress` section: step, worker, model, merge commit, and the review
  summary. The lead records decisions with `plan_update`, which appends dated
  entries to `## Decisions` and may edit context and add steps. Neither edit
  rewrites what is already recorded.
- **Plans close on their own.** When every step is done, no task stamped with
  the plan is open, and no finding is open, TeamManager closes the plan:
  1. writes the team review report (`## Verdict: PASS`) to `.coding/reviews/`;
  2. runs the plan-kind gates (a `bug_fixing` plan needs a recorded regression
     test that resolves in the code graph);
  3. captures the PLAN digest (and BUG digest) into project memory;
  4. marks the plan closed and sends the lead a notice.

  If a gate fails, the plan stays open and the notice says what is missing.
  `plan_abandon` closes a plan without review and records why.
- **Board state.** Each plan file carries its own steps, progress, and
  decisions. `.coding/plans/board.json` lists the board plans with
  open/closed/abandoned status and the closing report path. The task ↔ step link
  stays on the worker records in `team.json`.

#### 6.2 Engine

- `src/app/plan_board.rs`: `PlanBoard` over the plans and reviews directories
  plus optional memory, knowledge, and code-graph handles. It creates, reads,
  lists, updates, ticks, unticks, closes, and abandons plans, reusing the plan-file
  parser/serializer and the `finish` capture helpers.
- TeamLead mode no longer wraps the Mnemo1 workflow state. The lead's tool
  surface is free, and the classic plan tools (`create_plan`, `update_plan`,
  `complete_step`, `finish`, `abandon_plan`) are hidden. The board tools
  replace them: `plan_create`, `plan_update`, `plan_show`, `plan_list`,
  `plan_step_done`, `plan_abandon`. The volatile tail lists the open plans with
  their progress. Turn end always stops: worker notices and the user drive the
  lead.
- Removed: the Planning/Complete team-tool gate, the `complete_step` gate, and
  the ledger-backed `finish` path. Mnemo1's Classic workflow is unchanged.

#### 6.3 Host and UI

- `team_dispatch` takes `plan_id` and `step`, validates them on the board,
  composes the brief, and stamps the worker. A merge ticks the step and tries
  to close the plan; a revert unticks the step. Ticks, closes, and blocked
  closes reach the lead as TeamManager notices.
- The workbench Plan tab shows the board: every open plan with its steps, done
  state, and the workers on each step, plus recently closed plans. Worker rows
  and thread headers show their plan and step.

#### 6.4 Landed decisions and verification

- **Plan quality.** `plan_create` applies `create_plan`'s resumability checks:
  context of at least 40 characters, a file path in every step, and a
  regression-test design in a `bug_fixing` plan's context. The schema asks for
  the why, the constraints, and the key decisions.
- **Hidden tools.** Besides the classic plan tools, the lead's `current_plan`
  is hidden; it only read the old single plan.
- **Dispatch.** The lead's `team_dispatch` requires `plan_id` and `step` and
  validates them before any record, branch, or worktree exists. The worker's
  first message is the board header, `---`, then the lead's brief. The record
  keeps the lead's brief separately (`opening_prompt` holds the composed text),
  so idempotency compares the lead's brief, model, effort, and plan step.
  The Control Room's manual dispatch goes through the same plan-checked path
  (`TeamRuntime::dispatch_on_plan`): plan and undone-step pickers, defaulting to
  the first open plan's first undone step. With no open plan it shows "No open
  plan — ask the lead to plan the work first." Plan-free dispatch exists only
  in tests.
- **Ticks.** A step ticks when at least one non-reverted merged task exists and
  none is open (Stopped counts as closed). Revert unticks a step only when no
  other merged task remains. The tick, close, and revert hooks live in
  `TeamRuntime::merge`/`revert_merge`, so the tool and the GUI share them.
  Board errors never fail a merge; they reach the lead as a notice.
- **Lead bookkeeping.** The clean-lead check ignores `.coding/plans/`,
  `.coding/knowledge/`, and `.coding/reviews/`, so closing reports do not block
  the next merge.
- **Plan-file format.** `## Decisions` and `## Progress` are additive sections;
  old plan files parse and serialize byte-identically. Mnemo1 can open a board
  plan when it has no plan stack of its own.
- **Verification.** `cargo test --workspace`: 3555 passed, 0 failed.
  `cargo test -p mnemo-app --features mnemo2 --lib mnemo2`: 93 passed. The
  `mnemo2` binary builds. `npm --prefix mnemo2 test`: 176 passed; `check`: 0
  errors and 0 warnings; `build`: passed. No live-provider run has driven a
  plan through dispatch, merge, and close yet.

#### 6.5 Seam hardening (from the first live team run)

A live run with two parallel steps, a game engine and its UI, broke at the seam between them: the UI used a default import of a named export. That run also showed the lead storing execution state in Context, and working out the `team_review` finding format by trial and error. The fixes:

- **Interfaces.** Plans have an additive `## Interfaces` section for contracts (exports, signatures, data shapes, file ownership). It is set via `plan_create.interfaces` and `plan_update.interface`, and `brief_for` copies it into every worker brief.
- **Contract gate.** `PlanBoard::contract_gate` refuses a dispatch whose step shares a file with another step of the same plan that is still in flight, unless the Interfaces section names that file. The check runs in `dispatch_locked`, before anything is provisioned.
- **Integration check.** Dispatching a step while another step of the same plan is in flight appends an "Integration check" step once (`ensure_integration_step`), with a Decisions line explaining why. The plan cannot close until that step is done.
- **Progress, not Context.** `plan_update.progress` adds a dated line to `## Progress`. Context is stable intent, because briefs carry it and never carry Progress.
- **Status is pulled, not pushed.** `team_status` takes an optional `plan_id`. The lead's per-turn summary carries no live worker status.
- **Milestone notices.** A worker's completed turn notifies the lead with its plan step and the next move: review, close the findings, or merge.
- **Review format.** `team_review` publishes the full finding schema with its enums and an example. `parse_findings` maps aliases (`path`→`file`, `description`→`detail`, `blocking`→`high`), defaults `status` to `open`, and derives a stable id from the file and title. It reports every problem in one error, with `findings[i].field` paths and the allowed values.
- **Plan-scoped memory.** A memory that names a plan id (adjacent to the word "plan") or a branch is filed Episodic, not Semantic, and tagged in `Memory.data`. When the plan closes, `memory::plan_scope::settle_plan_memories` quarantines that plan's notes and appends them to the closing report, and the lead is told to re-store their durable facts without plan or branch references.

## 12. References

- `agent.md` — Mnemo1-scoped git gate + Mnemo2 team merge policy.
- `src/headless/types.rs` — `EngineOptions`, `SpawnOptions.worktree`,
  `host_tools`, `guardrails`, `SessionCommand`, `SessionEvent`.
- `src/tool/agent/spawn_agent.rs` — parent-aware spawn, approval.
- `src/runtime/mod.rs` — `ParentAwareSpawner`, `DescendantTracker`.
- `src/app/worktrees.rs` — provision/remove worktree helpers, including the
  agent-shaped `agent_branch` / `provision_agent_worktree` /
  `remove_agent_worktree` (base-ref forked, branch kept on cleanup).
- `src-tauri/src/ipc/spawn.rs` — the worktree-binding spawn pattern.
- `src/config/projects.rs` — `ProjectRegistry` / `projects.toml`.
- `src/agent/prompt.rs` — the compiled APP_RULES line (agent-facing gate).
- `src-tauri/mnemo2-app/tauri.conf.json` + `src-tauri/src/bin/mnemo2.rs` — the separate
  Mnemo2 native host.
- `src-tauri/src/mnemo2/runtime.rs` — `LeadRuntime`: engine/policy/session,
  event pump, persistence, interrupt, resume-based model changes, and the
  lead-side team host-tool bridge.
- `src-tauri/src/mnemo2/team.rs` — `TeamRuntime`: worker records, worktree
  dispatch, per-worker event pumps, executor guardrails, slots/limits,
  pause/interrupt/stop/resume, and `team.json` recovery.
- `src-tauri/src/mnemo2/project.rs` — registry, canonical project identity,
  per-project state/preferences, creation, and instance attachment.
- `src-tauri/src/mnemo2/browser.rs` — native child-WebView2 navigation, URL/title
  events, real history, visibility, rect coalescing, and external-open fallback.
- `src-tauri/src/mnemo2/workbench.rs` — read-only worker deltas and guarded
  screenshot reads; no agent-tool calls or external text conversion.
- `src/headless/session.rs` — `AgentSession::plan()`, the exact per-session
  active-plan snapshot used by the lead runtime.
- `src-tauri/build.rs` — test-target Tauri resource linking for the COMCTL32 v6
  manifest required by native dialog APIs.
- `src-tauri/capabilities/mnemo2.json` — the shell, dialog, window, and
  notification permissions for Mnemo2's own application identity.
- `src-tauri/src/mnemo2/ipc.rs` — typed lead/team/project commands and the
  `mnemo2://lead-event` + `mnemo2://team-event` forwarders.
- `mnemo2/src/lib/leadEngine.ts` — native lead transport.
- `mnemo2/src/lib/leadState.ts` — snapshot restore and live event reducer.
- `mnemo2/src/lib/teamEngine.ts` — native team transport.
- `mnemo2/src/lib/teamState.ts` — team snapshot restore, generation-tagged event
  reduction, stale-event rejection.
- `mnemo2/src/lib/projectEngine.ts` — native project registry/folder-picker
  transport.
- `mnemo2/src/lib/runtime.ts` — shared `isTauriRuntime` detection plus the
  `probeMnemo2Host` handshake (`mnemo2_host_ping`), which distinguishes the real
  Mnemo2 host from any other Tauri WebView (Mnemo1's Browser tab is one, and its
  IPC never registered these commands — Tauri answers `not allowed by ACL`).
  Kept separate so mocking one transport cannot break another.
- `mnemo2/src/test/fixtures.ts` — test-only fixture data; production imports none
  of it.
- `mnemo2/src/components/LeadModelPicker.svelte` — config-backed lead model
  picker used by the composer and project settings.

## 13. Verification (Phase 4)

- Full-delta self-review closed the COMCTL32 test-host manifest, browser
  snapshot/address races, async lifecycle cleanup, retained-branch review
  refresh, and sensitive-file preview/counting issues. Large raw patches keep
  their real Git totals while returned lines and files remain bounded. This was
  a self-review; no independent reviewer is claimed.
- `npm --prefix mnemo2 test`: 25 test files, 106 tests passed.
  `npm --prefix mnemo2 run check`: zero errors and warnings.
  `npm --prefix mnemo2 run build`: passed, including the production no-mock
  import check.
- `cargo test -p mnemo-app --features mnemo2 --lib`: 56 tests passed.
  `cargo build -p mnemo-app --features mnemo2 --bin mnemo2`: passed.
  `cargo test --workspace`: all workspace tests passed. `git diff --check`
  passed. The workbench file was formatted individually; no workspace-wide
  formatter was run.
- Windows native smoke used an isolated local, keyless configuration and a
  real registered test project. The real engine opened, the native WebView2
  displayed a local page with its actual title and body, dialogs hid the child
  surface, and the narrow workbench became a full-pane overlay. Worker slots
  and Browser preferences survived restart, a second instance showed the
  incumbent PID and required confirmation, and clean exits released the
  project marker. No prompt or worker dispatch was sent, so this was not a
  live-provider turn or a live-notification test.
- Legacy shared `lead/` state is preserved but not auto-migrated: its records
  have no project identity, so assigning them to a current project would be
  unsafe. New state is read only from the canonical project's own directory.
- The unrelated existing edits in `.coding/backlog.jsonl` and
  `.coding/plans/e3fcd987.md` are preserved and excluded from the Phase 4
  commit. No push or merge into `main` is part of this verification.
