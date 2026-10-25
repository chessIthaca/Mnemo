# Mnemo2 — Lead + Team (design v0.2)

Status: Phase 4 implemented — the native host runs one real streaming lead
session and a real parallel worker team on one shared headless engine, with
per-worker branches and linked worktrees, durable reviews/findings, exact-tip
checks, serialized local merge/revert, and cumulative telemetry/token budgets.
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
finding blocks merge and keeps the worker in **In review** / **Fixing**.

The lead's review is itself a cost center. For a large diff it may spawn a
read-only reviewer to pre-read and return findings (cheaper than pulling the
whole diff into the lead's long context), then triage and fix; for small diffs
it reviews directly.

**Check binding.** The checks precondition binds to the **branch tip being merged**. After a `review_fix` or worker fix commit, the lead runs the affected scope in the worker worktree and records real outcomes through `team_check`; TeamManager stamps the server-observed current tip. The affected checks must pass and receive a fresh delta review before the merge card becomes mergeable. For this repository the check scope is `cargo test --workspace`.

## 4. Auto-merge

When the review is closed and the worker's checks pass, the TeamManager merges
the worker branch into the lead's **local working branch**:

- The lead and worker are idle and both working trees are clean. Review,
  checks, dispatch, messaging, stop/resume, and merge/revert share one operation
  lock, so exact-tip evidence cannot change after validation. A busy lead or
  worker is rejected with an actionable error rather than silently queued.
- The merge target is never `main`. If the project is on `main`, merge refuses
  until a `wt/*` working branch exists.
- Worker branches fork from the lead's working-branch tip at dispatch. A
  base that goes stale while the worker runs is not auto-rebased; the
  conflict-halt + route-back rule is the handling.
- A conflict aborts the merge (`git merge --abort`), marks the worker
  **Conflict**, and routes it back to the worker/lead. No auto-resolution.
- The merge record stores: commit, files, `+x −y`, checks, review verdict,
  worker model, cumulative tokens, and pricing-derived cost when available.
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

## 5. Budget and cost

The team is budget-gated, not per-action-gated.

- **Caps**: `DEFAULT_SLOTS = 3` live worker sessions,
  `DEFAULT_SPAWNS_PER_TURN = 4` delegations per lead turn, 64 worker turns and
  30 minutes per session, and a default cumulative 200,000-token budget per
  worker. The startup default is configurable, and an exhausted worker's budget
  can be raised only above its real cumulative usage. Hitting the token cap
  pauses the worker in **Needs you** and blocks continuation until it is raised.
  Per-project monetary budgets remain an open decision.
- **Tracked dispatch**: `team_dispatch` runs only when slots and the per-turn
  delegation cap allow it. Generic spawning is unavailable, so no approval path
  can create an untracked worker.
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
- **Telemetry**: per-worker model, cumulative prompt/completion/cached tokens,
  requests, elapsed time, and completed turns, all persisted across sessions.
  Cost is derived only from configured model pricing; unknown cost is never
  shown as zero. A worker budget editor and aggregate usage/cost chip sit beside
  **Stop all**; individual workers also support pause/resume.

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
- The lead chat also shows the lead's own real tool calls as compact cards:
  the JSON arguments, a bounded live output tail while the call runs, and the
  final result or error. Live `SessionEvent` call starts/deltas/results and
  persisted transcript tool calls both feed the same reducer; a host-tool call
  renders as team work in progress, not as an approval decision, and no tool
  activity is synthesized.
- Markdown retains the last committed frame while the newest source is rendered
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
- The tool-level gate (`GitTool::never_auto_for`,
  `ShellTool::never_auto_for`) remains for the agent; the TeamManager uses
  app-level git operations, not the agent's git tool.

## 10. Open decisions

- Per-project monetary budgets. Custom worker-slot defaults are persisted.
- Whether a fresh fixer subagent is ever preferable to the current original-worker
  re-tasking path.
- Reviewer pre-pass threshold (diff size/files/risk).
- Whether workers may message the user directly or only the lead.
- Main landing UX (PR button vs the existing `merge_to_main` skill).

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
  `lead_close`. One event channel, `mnemo2://lead-event`, carries
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
  `update_plan`, `finish`, `spawn_agent`, and `ask_user`, plus hard turn and
  wall-clock limits. Planning and review stay the lead's job.
- **Messaging:** the lead gets `team_dispatch` / `team_message` / `team_status`
  host tools; workers get `message_parent`, whose messages land in a lead inbox
  and render in the lead conversation. A worker's direct `spawn_agent` is denied
  by a `WorkflowPolicy::with_blocked_tools` veto, so every worker must be tracked
  by the team runtime. (The plan sketched `message_agent`/`review_fix`; the
  landed lead-side tools are `team_message` and `team_status`, with
  `review_fix` deferred to Phase 3 along with review itself.)
- **Scheduling:** configurable live-session slots (`DEFAULT_SLOTS = 3`) and a
  per-lead-turn delegation cap (`DEFAULT_SPAWNS_PER_TURN = 4`). Pause,
  interrupt, stop, and resume drive real `SessionCommand`s.
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
  messaging, stop/resume, and merge/revert. Merge requires an idle clean lead and
  worker, a non-`main` target, exact-tip passing checks, and closed findings.
  Conflicts abort with diagnostics and route back; guarded revert works only
  before landing. Merge records include diff stats, model, tokens, and cost when
  priced. Sessions close before worktree cleanup; branches remain for audit.
- **Budget and recovery:** cumulative real usage, turns, activity, elapsed time,
  configurable per-worker token budgets, pricing-derived cost, stop-all, and
  restart downgrade. `team.json` uses a backup-recoverable replacement path.
  Interrupted conflicts abort cleanly; committed merges are reconstructed only
  from verifiable Git parent evidence, with stale `MERGE_HEAD` state cleared
  only after a clean-tree check.
- **Native/UI:** typed review/fix/check/merge/revert/stop-all/budget commands and
  real worker/inline lead cards. Generic `spawn_agent` is hidden, Markdown is
  retained across streaming, and no placeholder success path remains.
- **Acceptance met:** no merge with an open/unevidenced finding or stale check;
  parallel tasks keep independent states; exact-tip evidence cannot race the
  operation lock; conflict recovery leaves a clean tree; and committed crash
  recovery never invents a merge result.

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
- **Per-project persistence:** `projects/<sha256-of-canonical-path>/` holds
  `lead.json`, `team.json`, `preferences.json`, and the engine workspace.
  Lead transcripts carry project identity and save time. Worker slots and
  workbench preferences survive restart, with backup recovery. Legacy shared
  `lead/` state is left untouched; older records without a project identity
  are not assigned automatically to a possibly different project.
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
