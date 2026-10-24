# Mnemo2 — Lead + Team (design v0.1)

Status: planning. No product code yet. This is the design of record for the
Mnemo2 frontend rebuild. It supersedes the earlier "spaces dashboard" sketch:
Mnemo2 is a single-project cockpit for a lead agent and a team of worker
subagents. The companion policy change is in `agent.md` under **Mnemo2 team
merge policy**.

## 1. Product shape

Mnemo2 is a **single-project desktop cockpit**. The start screen chooses one
project directory; the running instance is scoped to it. Working on two
projects means two instances (later: two windows).

- **Start screen** — recent projects from `projects.toml`, open folder, create
  project. Shows the branch, last session, and unfinished worker branches from
  a previous run with Resume / Review / Clean.
- **Lead chat** — the main conversation: planning, guidance, research, review,
  and merge records. The lead never streams worker tool logs into this chat.
- **Team board** — the per-project Mission Control: peek sheet + control room.
- **Worker thread** — a real conversation with one worker: its timeline, its
  messages to the lead, the lead's replies, its branch, diff, and review.
- **Workbench** — Browser, Diff, and Plan tabs. The browser is a native child
  webview on Windows (macOS fallback), not an iframe.

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

The lead's own edits are capped by the **tiny-work rule**: at most 20 changed
lines in one file, no new files, no schema/API/dependency/test changes, and no
cross-module refactor. The cap is enforced by an app-level external policy hook
on `file_edit` / `file_write` / `shell`; over-cap calls are denied with a
delegation nudge. Direct edits are logged as small cards in the lead chat and
counted in the cost meter. The lead may debug, run tests, read logs, and
research without a cap; the cap is about where implementation happens, not
about investigation.

Delegation is economic, not a hard lockout: do tiny/review/debug work directly;
delegate anything that needs exploration or touches more than one file; never
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

**Tiny findings — the lead fixes them.** A tiny finding is ≤20 changed lines in
one file, with no schema/API/dependency/test change. The lead calls the
`review_fix` host tool with a worker id, file path, and edit. The TeamManager
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

- **Caps**: max concurrent workers (default 3), max spawns per lead turn
  (default 5), per-worker turns/wall-clock/cost, and a per-project budget.
  Hitting a cap pauses the team and moves the item to **Needs you**; nothing
  runs away silently.
- **Spawn approvals**: the kernel's spawn approval is auto-answered by the
  TeamManager when the spawn fits the budget; when it does not, the approval
  stays pending and surfaces in Mission Control.
- **Model routing**: strong lead, cheap/default workers, configured reviewer;
  the lead can pick a cheaper model for mechanical work via `list_models`.
- **Executor workers**: one plan of record (the lead's), not one per worker.
- **Self-contained briefs**: the lead attaches the plan excerpt, paths,
  acceptance criteria, and scope limit; workers do not rediscover the repo.
- **Review economics**: review small diffs directly; pre-read large diffs with
  an optional reviewer; never spawn a worker for a read or an answer.
- **Telemetry**: per-worker model, tokens, cost, elapsed, turns; a budget chip
  in the top bar; a "Pause team" / "Stop all" control; a cost line for the
  lead's direct edits.

## 6. Mission Control

Mission Control is per project, not cross-project. It is an
exceptions-and-budget board, not an approval queue.

States: **Dispatching**, **Working**, **In review**, **Fixing**, **Merging**,
**Merged**, **Conflict**, **Failed**, **Cancelled**.

- **Peek** (380px, over the chat side): Needs you (conflict/failed/over-budget),
  In review, Working, plus the budget chip.
- **Control room** (a route): grouped board + detail pane. The detail shows the
  worker chat, its timeline, branch/worktree, diff stats, review findings, and
  actions (Reply, Open, Steer, Stop, Diff, Review, Merge record, Revert).
- The lead is not a row; it is the chat. The activity badge counts workers.
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

- Default budget and caps (concurrent workers, spawns per turn, per-worker
  cost, project budget).
- The exact tiny-edit cap (20 lines? 30?) and whether `review_fix` shares it.
- Fixer default: re-task the original worker vs spawn a fresh fixer.
- Reviewer pre-pass threshold (diff size/files/risk).
- Whether workers may message the user directly or only the lead.
- Team-state persistence path (app data vs project `.coding`).
- Main landing UX (PR button vs the existing `merge_to_main` skill).

## 11. Phases

0. **Visual prototype** — start screen, lead chat, Team board, budget chip,
   merge records, worker detail, browser shell; mock team. No core wiring.
1. **Real team** — TeamManager, worktree-aware spawner, live board, serialized
   auto-merge queue.
2. **Conversation** — `message_agent` / `message_parent` / `team_status` /
   `review_fix` host tools, worker chat, steer/answer/prompt delivery, lead
   tiny-work policy hook, cost telemetry.
3. **Review quality** — optional reviewer, conflict handling, revert, budget
   enforcement, finding records.
4. **Browser/workbench + settings + persistence polish.**

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
