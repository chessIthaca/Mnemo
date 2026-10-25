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
cumulative worker telemetry and token budgets. No review or merge result is
simulated.

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
outputs to `target/mnemo2-dist`, distinct from the Mnemo1 frontend on 5179. The
native host uses its own `src-tauri/mnemo2-app/tauri.conf.json` and binary; it does not
replace or extend Mnemo1's IPC surface. It also clears inherited
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, so it never reuses Mnemo1's CDP port 9222.

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
- **State.** App-owned state lives under the Tauri app-data directory for
  `com.mnemo.mnemo2`, in `projects/<sha256-of-canonical-project-path>/`.
  `lead.json` holds the model spec, project identity, transcript, and save time;
  `team.json` holds the worker ledger and live-session cap; `preferences.json`
  holds workbench preferences. `workspace/` contains the per-session plan
  directory. These are not tracked project files. Endpoints and models come
  from the normal Mnemo config directory (`$MNEMO_HOME` or `~/.mnemo`).
- **Models.** The picker lists the configured `endpoints.toml` model specs for
  the selected project. The lead header, composer, and project settings show
  the actual serving model id reported by the session. A model change is allowed
  only while idle and resumes the saved transcript.
- **Interaction.** The composer sends through the real session. While a turn
  runs, the send button becomes a stop button; `lead_interrupt` cancels the
  generation without closing the session. Errors render inline with **Retry**;
  pending approvals and questions render inline with their actions.
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
- **Executor surface.** Workers run with `ToolSelection::all()` minus
  `create_plan`, `update_plan`, `finish`, `spawn_agent`, and `ask_user`; planning
  and review stay the lead's job. Worker sessions get hard turn and wall-clock
  limits.
- **Events.** Worker events are forwarded on `mnemo2://team-event` as
  `{ worker_id, generation, event }`. The frontend ignores stale generations.
- **Commands.** `team_snapshot`, `team_dispatch`, `team_message`, `team_pause`,
  `team_interrupt`, `team_stop`, `team_resume`, `team_review`,
  `team_record_check`, `team_resolve_finding`, `team_review_fix`,
  `team_route_fix`, `team_merge`, `team_revert_merge`, `team_stop_all`,
  `team_set_slots`, `team_set_token_budget`, and `team_messages`.
- **Host tools.** The lead gets `team_dispatch`, `team_message`, `team_status`,
  `team_review`, `team_check`, `team_resolve_finding`, `review_fix`,
  `team_route_fix`, `team_merge`, and `team_revert_merge`. Generic `spawn_agent`
  is removed from the advertised lead registry and denied as a fallback. Workers
  get `message_parent`, whose messages land in the lead conversation.
- **State.** `team.json` in the project's app-owned state directory is the
  worker ledger and stores the live-worker cap across restarts.

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
  visibility handling. There is no iframe or CDP browser path. Other platforms
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
  Duplicate transitions are suppressed, and nothing is emitted from fixtures.
- **Accessibility** includes modal focus management, keyboard resizing,
  readable small text, narrow-layout spacing, and a full-height workbench
  overlay with a back button.

## Tour

1. Pick a registered project, or **Open folder** to register a real one.
2. Send a message to the lead. Responses stream into the conversation and render
   sanitized Markdown with tables and code.
3. Choose the lead model from the composer or **Project settings**. The picker
   is disabled while a turn is running.
4. If the lead asks for approval or asks a question, resolve it inline. Use the
   stop button to cancel a running generation.
5. Open **Team** and dispatch a task (or let the lead call `team_dispatch`). Each
   worker gets its real branch, worktree, model, and workflow state; tasks
   advance independently. Filter **Needs you / Active / Done**; the header shows
   live sessions against the configured slots.
6. Open a worker. **Thread** sends a real message (a steer while busy).
   **Review** shows exact-tip checks, findings, review/merge records, and real
   usage. Apply a one-file `review_fix` with exact text, route larger findings
   back to the worker, and close findings with evidence or a written `wontfix`.
7. Once every finding is closed and checks pass on the reviewed tip, merge into
   the local non-`main` lead branch. **Revert** is available before landing.
   **Stop all** closes live worker sessions; **Worker token budget** raises a
   worker's cumulative cap above its actual usage.
8. Open the workbench from the top bar, then switch to **Diff**. Use **Browser /
   Diff / Plan**; the pane is resizable on desktop and full-screen on narrow
   layouts.

## Review and merge policy

`src/lib/review.ts` mirrors the native merge-eligibility gate:

- **Lead tiny-work cap** — either up to **5 files × 10 changed lines**, or a
  **single file with a larger edit**. Both exclude new files, schema/API/
  dependency/test changes, and cross-module refactors.
- **Merge eligibility** — the worker must be in review, every finding must be
  fixed or `wontfix` with a written justification, and every branch-tip check
  must have passed. A fix or fixer commit replaces the check result and must be
  re-run before merge.
- The lead is the reviewer of record. An independent reviewer is never a
  substitute PASS gate.

## What is real

- **Real (native lead):** streaming, model identity and picker, cancellation,
  transcript persistence/restore, and approvals/questions.
- **Real (native team):** worker sessions on the same engine, per-worker branch,
  worktree, brief, model, status, transcript, checks, findings, review/merge
  records, and pending interaction. Every control drives the native runtime;
  generic spawning is unavailable so no untracked worker can exist.
- **Real review and integration:** findings close only with fixed evidence or a
  written `wontfix`; checks and review bind to the exact worker tip. `review_fix`
  edits one existing non-test source file through a canonical, symlink-free path,
  commits only that scoped change, and invalidates old evidence. Larger fixes
  return to the tracked worker. Merge is
  serialized into the local non-`main` lead branch, aborts conflicts without
  auto-resolution, never pushes, and supports guarded revert before landing.
  Successful merge closes the worker session, removes the worktree, and keeps
  the branch for audit.
- **Real limits and telemetry:** live-session slots (`DEFAULT_SLOTS = 3`), a
  per-lead-turn delegation cap (`DEFAULT_SPAWNS_PER_TURN = 4`), and cumulative
  per-worker token budgets (default 200,000). Requests, prompt/completion/cached
  tokens, turns, and elapsed time come from real events. Cost is shown only when
  model pricing is configured; token usage is always available.
- **Real recovery:** running workers reopen as interrupted, and `team.json` has
  a backup-recoverable replacement path. An unfinished conflict is aborted and
  verified clean; a completed merge is recovered only when its second parent
  proves the reviewed worker tip. Stale `MERGE_HEAD` state is cleared only after
  a clean-tree check. Ambiguous recovery routes back for inspection.
- **Native workbench and lifecycle:** actual lead plans, worker Git deltas,
  a native WebView2 Browser with real navigation/title events, sandboxed image
  previews, per-project state and preferences, guarded cleanup, instance
  warnings, close confirmation, and real worker notifications. No stand-in
  browser, plan, project, worker, or merge-success path remains.

## Layout notes

- Warm-white tokens, Inter UI, Source Serif 4 prose; prefers-reduced-motion is
  respected.
- Conversation-first: the lead chat and worker **Thread** are primary; review,
  checks, timeline, and merge are secondary disclosures.
- No project-wide run pipeline or phase diagram. Each task derives its own
  workflow from status, checks, findings, and merge state.
- Every lead/worker surface shows a model badge; native lead surfaces use the
  real configured model picker.
- Workbench lives beside the lead chat only; opening Mission Control hides it,
  mirroring the native-webview overlay rule. On narrow layouts it becomes a
  full-height overlay, with bottom navigation space reserved.
- The team peek is anchored over the chat side.

## Roadmap status

See `.coding/design/mnemo2.md` for the full design and acceptance criteria:

1. **Lead engine + model registry** — complete: one real streaming lead session,
   config-backed model picker, project lead model.
2. **Parallel team runtime** — complete: shared engine, per-task
   branches/worktrees/briefs/models, worker messaging, slots, restart recovery.
3. **Review, merge, recovery, cost** — complete: durable findings, tiny-work
   fixes, tracked fixer routing, exact-tip checks, serialized local merge/revert,
   conflict and crash recovery, cumulative token budgets, and pricing-aware cost.
4. **Native shell + workbench + persistence** — complete: real Diff/Plan tabs,
   native browser, isolated per-project state and preferences, safe recovery,
   notifications, accessibility, and narrow layouts. Remaining product choices
   are recorded in the design's open-decisions section.
