# Mnemo2 — real lead, review, and team integration

A standalone Svelte 5 workspace for the Mnemo2 **lead + team** cockpit. In the
native Tauri host the lead conversation and worker team are real: one shared
headless-engine `Engine` backs every session, each worker gets its own
`wt/agent-*` branch and linked git worktree, and messages route both ways. The
browser-only Vite session has no engine behind it, so it shows an explicit
native-shell-required state instead of simulating anything.

The product shape is unchanged from Phase 0b: the lead owns a workflow for each
task, tasks are independent, and lead and worker models are visible wherever an
agent appears. Native Phase 3 adds durable reviews/findings, exact-tip checks,
tiny review fixes and tracked fixer routing, serialized local merge/revert, and
cumulative worker telemetry without per-agent token or cost budgets. No review
or merge result is simulated.

## Run

From the repository root:

```bash
npm install
npm run dev:mnemo2       # http://127.0.0.1:5180 — browser-only preview (needs the native shell)
npm run check:mnemo2     # svelte-check (0 errors / 0 warnings expected)
npm run test:mnemo2      # Vitest interaction, adapter, reducer, and policy tests
npm run build:mnemo2     # -> target/mnemo2-dist
```

Native lead host (run Vite in a first terminal):

```bash
npm run dev:mnemo2         # terminal 1: Vite on 5180
npm run dev:mnemo2:native  # terminal 2: Tauri host + real lead
npm run build:mnemo2:native
```

`mnemo2` is a separate npm workspace (`mnemo2/`). Vite uses port **5180** and
outputs to `target/mnemo2-dist`, distinct from the Mnemo1 frontend on 5179.
The native host uses its own `src-tauri/mnemo2-app/tauri.conf.json` and
binary; it does not replace or extend Mnemo1's IPC surface. It also clears
inherited `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, so it never reuses
Mnemo1's CDP port 9222.

The native host is the `mnemo2` Cargo feature. The default `cargo test
--workspace` compiles Mnemo1 alone; `npm run test:mnemo2:native` runs the
feature-enabled Mnemo2 lib tests.

## Native lead

- **Runtime.** `LeadRuntime` starts the existing `mnemo::headless::Engine`,
  registers `WorkflowPolicyFactory` as `mnemo.workflow`, and opens one lead
  `AgentSession` under `ApproveViaApi` / `ApproveEachAction`.
- **Events.** Every session event is forwarded on one Tauri channel,
  `mnemo2://lead-event`, as
  `{ generation, event: { agent, event: { kind, … } } }`. A model switch resumes
  a fresh session and advances `generation`; the frontend ignores stale
  generations.
- **Commands.** `lead_open`, `lead_snapshot`, `lead_prompt`, `lead_interrupt`,
  `lead_set_model`, `lead_approve`, `lead_answer`, `lead_close`.
  `lead_prompt` takes `{ text, images }` — base64 data-URL attachments ride
  the turn like a headless prompt's images, the `images` key may be omitted
  by legacy callers, and a busy lead receives the submission as a queued
  steer (`SessionCommand::Steer`, same payload) injected at the next safe
  turn boundary instead of an error.
- **State.** All of a project's Mnemo2 state lives inside the project, in
  `.coding/mnemo2/`, so deleting the project deletes its history.
  `lead.json` holds the model spec, reasoning effort, project identity,
  transcript, and save time; `team.json` holds the worker ledger and
  live-session cap; `preferences.json` holds workbench preferences.
  `workspace/` contains the per-session plan directory. The folder carries its
  own `.gitignore` (`*`): its contents are per-machine (absolute worktree
  paths, multi-MB transcripts), and a lead save must never dirty the checkout
  that merges and cleanup require clean. Because a repository or archive can
  ship its own `.coding/mnemo2/`, the folder is trusted only when stamped
  (`owner`) with this machine's random id (`state-owner-id`, the one Mnemo2
  file kept in the app-data dir): an unstamped or foreign-stamped folder that
  holds state is refused, never resumed, and the start screen says why. The
  `.coding` and `mnemo2` folders must be real folders (no links/junctions)
  resolving inside the project, and the `.gitignore` is rewritten on every
  open. Earlier builds kept this state in the app-data directory
  (`com.mnemo.mnemo2/projects/<sha256>/`); it is no longer read. Endpoints and models come from the normal Mnemo config directory
  (`$MNEMO_HOME` or `~/.mnemo`).
- **Models.** The picker lists every configured `endpoints.toml` model as
  `endpoint / model` (the configured default is marked), so equal model ids on
  different endpoints never look alike. Beside it, a reasoning-effort picker
  offers that model's allowed efforts (`reasoning_efforts`, or the standard
  list plus `off`) and a "default" entry showing its configured effort; it is
  hidden for endpoints without `supports_reasoning_effort`. The lead's model
  and effort persist in `lead.json`; a change is allowed only while idle and
  resumes the saved transcript (`lead_set_model { spec, reasoningEffort }`).
  Mission Control's dispatch bar uses the same picker, and `team_dispatch`
  takes an explicit `reasoning_effort` validated against the chosen model and
  kept on the worker record for every later session.
- **Interaction.** The composer sends through the real session. While a turn
  runs, the send button becomes a stop button; `lead_interrupt` cancels the
  generation without closing the session. Errors render inline with **Retry**;
  pending approvals and questions render inline with their actions. The lead
  snapshot carries the question text, options, and approval tool name (the
  engine snapshot keeps only the id), and while a question is open the
  composer sends a free-form answer.
- **Restore.** The transcript is persisted when a turn finishes or parks and is
  resumed on the next native open.
- **Projects.** `project_list` / `project_add` / `project_pick_folder` read and
  write the shared `projects.toml` registry. A picked folder is canonicalized,
  must be a directory, and gets a folder-derived name (duplicate basenames get a
  numeric suffix). No seeded project paths remain.

## Native team

- **One engine.** `TeamRuntime` spawns workers on the *same* `Arc<Engine>` as the
  lead — never a second engine.
- **Worktrees.** Each worker gets `wt/agent-<id8>` at
  `<project>/.worktrees/agent-<id8>`, forked from the lead's current branch tip
  (not `main`). The branch name is unique per worker; provisioning fails loudly
  on a collision.
- **Executor surface.** Workers use a dedicated Mnemo2 executor prompt, not the
  original Mnemo workflow's read-only reviewer profile. They run with
  `ToolSelection::all()` minus `create_plan`, `update_plan`, `finish`,
  `spawn_agent`, and `ask_user`; planning and review stay on the lead's main
  thread. Workers do the bulk of the implementation: the lead's team rules tell
  it to delegate every self-contained task, never to brief a worker as
  read-only or content-only, and to pick each worker's model per task. Worker
  sessions run without any session limit (no request, token, cost, or
  wall-clock cap); the lead or user stops a worker explicitly. **Audit
  workers** (independent reviewers for high-risk work) are read-only: no file
  mutation, no commits, no merge, no spawning, no user questions, and no plan
  steps.
- **Events.** Worker events are forwarded on `mnemo2://team-event` as
  `{ worker_id, generation, event }`. The frontend ignores stale generations.
  The engine bus is shared by the lead and every worker, so each pump keeps
  only its own session's events (`SessionEvent::is_from`); a worker's
  auto-answered `message_parent` call never counts as needing the user.
- **Worker thread.** A worker's Thread renders like the lead conversation:
  the brief and lead replies, the worker's Markdown, its `message_parent`
  reports as messages, every tool call as an activity card, and `[harness …]`
  notes labeled as such (system prompts are hidden). Streamed text, tool calls,
  output, and results appear live from the event stream; when the turn's
  transcript is saved, the saved conversation replaces the live tail. Stream
  deltas never refetch the team ledger. The engine keeps the per-request
  workflow tail and context footer out of every export and strips any an older
  build leaked into a saved transcript on resume.
- **Commands.** `team_snapshot`, `team_dispatch`, `team_message`, `team_pause`,
  `team_interrupt`, `team_stop`, `team_resume`, `team_compact`, `team_review`,
  `team_record_check`, `team_resolve_finding`, `team_review_fix`,
  `team_route_fix`, `team_merge`, `team_revert_merge`, `team_stop_all`,
  `team_set_slots`, `team_delete_preview`, `team_delete`, and `team_messages`.
- **Host tools.** The lead gets `team_dispatch`, `team_message`, `team_status`,
  `team_stop`, `team_resume`, `team_compact`, `team_review`, `team_check`,
  `team_resolve_finding`, `review_fix`, `team_route_fix`, `team_audit_dispatch`,
  `team_merge`, and `team_revert_merge`. Dispatch accepts a caller-stable worker
  ID, an optional `review_risk` (`standard`/`high`), and returns a
  model-facing JSON receipt with its real ID/status. Its `model` parameter is
  an enum of the exact configured `endpoint/model` specs. Retrying that ID with the
  same brief/model cannot duplicate the worker. Unknown explicit models are
  rejected before provisioning. TeamManager state is authoritative; the lead
  must not infer lifecycle from Git/files or edit `team.json`. Generic
  `spawn_agent` is removed from the advertised lead registry and denied as a
  fallback. Workers get `message_parent`, whose messages land in the lead
  conversation and are forwarded as attributed lead notices.
- **State.** `team.json` in the project's app-owned state directory is the
  app-owned worker ledger and stores the live-worker cap across restarts. Only
  TeamManager mutates that state.
- **Deleting workers.** **Delete** in a worker's detail, or **Delete old** in
  Mission Control (merged, cancelled, stopped, interrupted, and failed workers),
  first shows a `team_delete_preview` of what goes: the record, transcript,
  telemetry, review, inbox messages, checkout, and `wt/agent-*` branch. A live
  session is stopped first; a merge in progress blocks deletion. Unmerged
  commits and uncommitted changes are discarded only when the confirmation
  says so (`team_delete` with `discard_work`). A branch that another worker
  record still uses (legacy ids sharing an 8-character prefix) is kept until
  its last record is deleted. The lead has no delete tool.

## Native workbench and project lifecycle

- **Diff** reads the selected worker's real Git delta, including working-tree
  and untracked changes. Merged workers use the recorded merge parent and
  reviewed tip; cleaned branches remain reviewable. Binary, sensitive, and
  truncated content is explicitly labelled. Worker-to-Diff navigation uses the
  same selected worker, never a hardcoded branch.
- **Plan** shows the active lead plan's real title, goal, and checklist from the
  current session's plans directory. It does not read a global plan stack or
  invent worker assignments.
- **Browser** is a native WebView2 child on Windows, with address navigation,
  real back/forward/reload, URL/title synchronization, and modal/resize
  visibility handling. Because the pane is an OS window above the whole UI,
  the store only lets it show on the open Browser tab of the Lead view with no
  team drawer or screenshot over it, and every visibility request carries a
  sequence number (`browser_set_visible { visible, seq }`) so a late, older
  "show" can never override a newer "hide". There is no iframe or CDP browser
  path. Other platforms
  offer an external-open fallback. Worker screenshot chips open actual sandbox
  PNG previews, and chat links open the Browser pane.
- **Persistence** isolates state by a hash of the canonical project path.
  Transcripts carry project identity and timestamps; worker slots and
  workbench preferences survive restart with backup recovery. The older shared
  `lead/` directory is left untouched. Its records have no project identity,
  so ambiguous legacy state is not automatically assigned to another project.
- **Lifecycle** includes real Git/session metadata, New project, Resume,
  Review, and Clean. Opening a second live instance on one project requires
  confirmation. Closing active work asks before stopping it. Cleanup removes
  only explicitly confirmed inactive clean worktrees; branches and dirty or
  conflicting work remain available for review. Cleaned workers are review-only.
- **Notifications** use the official Tauri notification plugin for real worker
  completion, review/input requests, failures, and conflicts when unfocused.
  Blocked-worker messages and failures also send deduplicated TeamManager
  notices to the actual lead session; they render as team activity, not human
  input. Duplicate transitions are suppressed, and nothing comes from fixtures.
- **Accessibility** includes modal focus management, keyboard resizing,
  readable small text, narrow-layout spacing, and a full-height workbench
  overlay with a back button.

## Tour

1. Pick a registered project, or **Open folder** to register a real one.
2. Send a message to the lead. Responses stream into the conversation and render
   sanitized CommonMark/GFM with task lists, tables, and highlighted code.
3. Choose the lead model from the composer or **Project settings**. The picker
   is disabled while a turn is running.
4. If the lead asks for approval or asks a question, resolve it inline. Use the
   stop button to cancel a running generation.
5. Open **Team** and dispatch a task (or let the lead call `team_dispatch`). Each
   worker gets its real branch, worktree, model, and workflow state; tasks
   advance independently. The lead uses a stable worker ID so an uncertain
   dispatch can be retried without duplication. Filter **Needs you / Active /
   Done**; the header shows live sessions against the configured slots.
6. Open a worker. **Thread** sends a real message (a steer while busy).
   **Review** shows exact-tip checks, findings, risk, structured review evidence,
   audit linkage, and merge records. Apply a one-file `review_fix` with exact
   text, route larger findings back to the worker, and close findings with
   evidence or a written `wontfix`. Before trusting the worker's summary, use
   **Diff** for your own unanchored pass. **Compact** compacts an idle worker or
   resumes a stopped/interrupted worker before compacting; the actual completion
   or error arrives from the session.
7. For a high-risk worker, `team_audit_dispatch` a fresh read-only audit at the
   current tip, then record the audit link and your adjudication in
   `team_review`. Once every finding is closed, the evidence is complete, any
   required audit matches the tip, and checks pass, merge into the local
   non-`main` lead branch. **Revert** is available before landing. **Stop all**
   closes live worker sessions while retaining their work. Token and cost
   figures are telemetry; there is no agent token or cost budget.
8. Open the workbench from the top bar, then switch to **Diff**. Use **Browser /
   Diff / Plan**; the pane is resizable on desktop and full-screen on narrow
   layouts.

## Review and merge policy

`src/lib/review.ts` mirrors the native merge-eligibility gate.

**Acceptance scenarios first.** A plan that will dispatch `review_risk: "high"`
carries a `## Acceptance scenarios` section with at least one observable bullet
(a command, a persistence state, a concurrency outcome, a security property, or
a UI behavior) written before implementation. Other plans are encouraged to
include it. `team_dispatch` rejects a high-risk dispatch without it, and the lead
re-checks those scenarios on the integrated tree before `finish`.

**Unanchored pass, then evidence.** The lead reads the target diff first
(`git_read` op `diff`/`show`, the **Diff** tab, or `git diff <base>..<tip>`) and
forms an opinion *before* reading the worker's summary, findings, or test
claims. It then records one evidence object per review:

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

`intent` says what the change was meant to do and whether the diff matches the
plan; `execution` covers the real changed call sites and error paths;
`integration` covers contracts, persistence, concurrency, and neighbouring
modules; `test_quality` asks whether tests genuinely exercise the change
(including a red-before/green-after regression test for a defect fix);
`residual_risk` names what remains unverified or assumed. A **standard** review
requires `unanchored_pass: true` and all five assessment strings nonempty. A
no-findings result still needs a substantive rationale — "looks good" or a
restated summary fails the gate.

**Risk and independent audit.** The lead marks shared contracts, persistence,
concurrency, security, and large or cross-cutting changes
`review_risk: "high"`. A high-risk worker cannot merge on lead review alone.
`team_audit_dispatch { worker_id, reviewer_id, model?, reasoning_effort?, brief? }`
creates a fresh read-only audit worker against the implementation worker's
branch/tip and plan context, with no plan step and **without** the
implementation worker's summary or conclusions. The audit worker cannot edit
files, commit, merge, spawn, ask the user, or tick a plan step; its completion
notice asks the lead to adjudicate the target review, not merge the reviewer.
Fresh context is required for independence; a different model is preferred when
available but not required. The lead links the completed audit as
`independent_reviewer_id` and writes `audit_adjudication` — what was accepted or
rejected and why. The host validates kind, target, exact tip, and terminal
non-failed state at review and merge.

**Staleness and legacy records.** A fix commit (`review_fix`, a worker fix, or a
routed fixer) invalidates the prior lead review and makes the previous audit tip
stale; evidence and any audit must be repeated at the new tip. Worker records
carry `kind` (`"implementation"`/`"audit"`, default implementation),
`review_risk`, and optional `audit_target`/`audit_tip`. Legacy ledger records
deserialize as implementation/standard with no evidence, so they need a fresh
review — and an audit where high risk — before merge.

**Merge eligibility.** The worker must be in review; every finding must be fixed
or `wontfix` with a written justification; the structured review evidence must be
complete for the current tip; a high-risk worker must have a completed audit
linked to that same tip and a nonempty adjudication; and every branch-tip check
must have passed. A fix or fixer commit replaces the check result and must be
re-run before merge. The lead is the reviewer of record and remains accountable
for the adjudication. This is Mnemo2's policy; Mnemo1's spawned read-only
reviewer workflow is unchanged.

## What is real

- **Real (native lead):** streaming, model identity and picker, cancellation,
  transcript persistence/restore, and approvals/questions.
- **Real (native team):** worker sessions on the same engine, per-worker branch,
  worktree, brief, model, status, transcript, checks, findings, review/merge
  records, and pending interaction. Every control drives the native runtime;
  generic spawning is unavailable so no untracked worker can exist.
- **Real review and integration:** findings close only with fixed evidence or a
  written `wontfix`; the lead's unanchored diff pass and structured evidence,
  risk, and audit linkage bind to the exact worker tip. High-risk work requires a
  fresh read-only audit at that tip plus a lead adjudication. `review_fix` edits
  one existing non-test source file through a canonical, symlink-free path,
  commits only that scoped change, and invalidates old evidence. Larger fixes
  return to the tracked worker. Merge is
  serialized into the local non-`main` lead branch, aborts verified conflict-only
  merges without auto-resolution, never pushes, and supports guarded revert
  before landing. Successful merge closes the worker session, removes a verified
  clean worktree, and keeps the branch for audit; new dirty work is retained.
- **Real limits and telemetry:** live-session slots (`DEFAULT_SLOTS = 3`).
  There is no per-lead-turn delegation cap (removed by user decision 2026-10-31)
  and no cumulative worker token or cost budget. Worker sessions have no
  request, token, cost, turn, or wall-clock limit.
  Requests, prompt/completion/cached tokens, turns, and elapsed time come from
  real events and never trigger a budget pause. Cost is shown only when model
  pricing is configured; unknown cost remains unknown, and usage is always shown.
- **Real recovery:** running workers reopen as interrupted, and `team.json` has
  a backup-recoverable replacement path. Conflict-only recovery can abort and
  verify clean; unrelated lead changes prevent automatic abort and remain in
  place for inspection. A completed merge is recovered only when its second
  parent proves the reviewed worker tip. Dispatch rollback deletes only a clean,
  newly provisioned branch whose tip still equals the dispatch base.
- **Native workbench and lifecycle:** actual lead plans, worker Git deltas,
  a native WebView2 Browser with real navigation/title events, sandboxed image
  previews, per-project state and preferences, guarded cleanup, instance
  warnings, close confirmation, and real worker notifications. No stand-in
  browser, plan, project, worker, or merge-success path remains.

## Layout notes

- Warm-white tokens, Inter UI, Source Serif 4 prose; prefers-reduced-motion is
  respected.
- Conversation-first: the lead chat and worker **Thread** are primary; review,
  checks, timeline, and merge are secondary disclosures. Human messages stay
  left-aligned and inline in exchange order. Prose is capped at 100 characters;
  code and tool details use the available width. Tool calls show compact human
  summaries with expandable real details and dotted consecutive-run connectors.
  Tool rows show real elapsed durations. Thinking with a live turn timer appears
  inline until the next tool call, and Stop stays available while busy or awaiting
  input. LLM errors and automatic retries stay visible at the conversation tail;
  terminal failures offer Retry and successful progress clears retry warnings.
- No project-wide run pipeline or phase diagram. Each task derives its own
  workflow from status, checks, findings, and merge state.
- Worker surfaces show a model badge; native lead surfaces keep the real
  configured model picker in the composer/settings without a redundant lead
  heading or lead-mark icons.
- Workbench lives beside the lead chat only; opening Mission Control hides it,
  mirroring the native-webview overlay rule. On narrow layouts it becomes a
  full-height overlay, with bottom navigation space reserved.
- The lead's "Waiting on …" and "Needs you: …" lines name live workers as
  inline buttons that open a worker output panel beside the chat: the same
  live Thread Mission Control shows (streamed messages, tool calls, screenshot
  links) plus a worker picker, model/status/plan header, a private steer box
  that sends `team_message`, and a Mission Control hand-off. A failed,
  conflict, stopped, or interrupted worker gets a banner with its error and
  Continue/Delete ways out. The panel shares the right side with the Workbench
  — opening one closes the other — and resets when the project changes or the
  worker vanishes from a sync.
- The team peek is anchored over the chat side.

## Roadmap status

See `.coding/design/mnemo2.md` for the full design and acceptance criteria:

1. **Lead engine + model registry** — complete: one real streaming lead session,
   config-backed model picker, project lead model.
2. **Parallel team runtime** — complete: shared engine, per-task
   branches/worktrees/briefs/models, worker messaging, slots, restart recovery.
3. **Review, merge, recovery, cost** — complete: durable findings, tiny-work
   fixes, tracked fixer routing, exact-tip checks, serialized local merge/revert,
   conflict and crash recovery, idempotent lifecycle controls, real compaction,
   cumulative usage telemetry, and pricing-aware cost.
4. **Native shell + workbench + persistence** — complete: real Diff/Plan tabs,
   native browser, isolated per-project state and preferences, safe recovery,
   notifications, accessibility, and narrow layouts. Remaining product choices
   are recorded in the design's open-decisions section.
