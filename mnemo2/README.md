# Mnemo2 — Phase 2 real parallel team

A standalone Svelte 5 workspace for the Mnemo2 **lead + team** cockpit. In the
native Tauri host the lead conversation *and* the worker team are real: one
shared headless-engine `Engine` backs the lead session and every worker
session, each worker gets its own `wt/agent-*` branch and linked git worktree,
and messages route both ways. The browser-only Vite session has no engine
behind it, so it shows an explicit native-shell-required state instead of
simulating anything.

The product shape is unchanged from Phase 0b: the lead owns a workflow for each
task, tasks are independent, and the lead and worker models are visible wherever
an agent appears. Review, merge, and fixer routing are Phase 3; those controls
are visibly disabled rather than faked.

## Run

From the repository root:

```bash
npm install
npm run dev:mnemo2       # http://127.0.0.1:5180 — browser mock fallback
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
- **State.** The app-owned state lives under the Tauri app-data directory for
  `com.mnemo.mnemo2`: `lead/lead.json` holds the model spec and transcript;
  `lead/workspace/` is the engine workspace state. It is not a tracked project
  file. Endpoints and models come from the normal Mnemo config directory
  (`$MNEMO_HOME` or `~/.mnemo`).
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
  `team_interrupt`, `team_stop`, `team_resume`, `team_messages`.
- **Host tools.** The lead gets `team_dispatch`, `team_message`, and
  `team_status`; workers get `message_parent`, whose messages land in the lead's
  inbox and render in the lead conversation.
- **State.** `team.json` in the app-data dir is the worker ledger.

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
6. Open a worker. The conversation opens first; **Thread** sends a real message
   (a steer while the worker is busy). **Pause**, **Resume**, and **Stop** drive
   the real session. **Review** shows the Phase 3 gate, disabled.
7. Open the workbench from the top bar, then switch to **Diff**. Use **Browser /
   Diff / Plan**; the pane is resizable on desktop and full-screen on narrow
   layouts.

## Phase 3 policy (not yet enforced)

`src/lib/review.ts` holds the rules the Phase 3 runtime will enforce:

- **Lead tiny-work cap** — either up to **5 files × 10 changed lines**, or a
  **single file with a larger edit**. Both exclude new files, schema/API/
  dependency/test changes, and cross-module refactors.
- **Merge eligibility** — the worker must be in review, every finding must be
  fixed or `wontfix` with a written justification, and every branch-tip check
  must have passed. A fix or fixer commit replaces the check result and must be
  re-run before merge.
- The lead is the reviewer of record. An independent reviewer is never a
  substitute PASS gate.

Until that runtime exists, the UI keeps these controls disabled and says so
rather than simulating a review or a merge.

## What is real vs. Phase 3

- **Real (native host):** the lead session, streaming, model identity and
  picker, cancellation, transcript persistence/restore, approvals/questions.
- **Real (Phase 2 team):** worker sessions on the same engine, per-worker
  `wt/agent-*` branch + linked worktree, brief, model spec, status, transcript,
  token usage, and pending interaction. Dispatch, message/steer, pause,
  interrupt, stop, and resume all drive real `SessionCommand`s. The lead
  delegates through the `team_dispatch` host tool; its direct `spawn_agent` call
  is denied so no untracked worker can exist.
- **Real limits:** live-session slots (`DEFAULT_SLOTS = 3`) and a per-lead-turn
  delegation cap (`DEFAULT_SPAWNS_PER_TURN = 4`).
- **Real recovery:** `team.json` under the app-data dir is the worker ledger. On
  restart, running workers become `interrupted` (or stay `needs_you` when a
  pending interaction survived); `team_resume` reattaches the saved transcript.
- **Phase 3 (disabled, not faked):** review findings, tiny-work fixes, fixer
  routing, branch-tip checks, and the serialized merge. Their buttons are
  disabled and the store reports "lands with the real review/merge runtime in
  Phase 3".
- **No git operations yet.** Stopping a worker removes nothing: its worktree and
  `wt/agent-*` branch stay for audit. Nothing merges into the lead branch until
  Phase 3.
- **Browser tab.** Still a labelled stand-in; the native WebView2 pane is
  Phase 4. Do not launch a separate Chrome/WebView instance for UI checks.

## Policy encoded in the prototype

The mock still enforces the product rules, in `src/lib/review.ts`:

- **Lead tiny-work cap** — either up to **5 files × 10 changed lines**, or a
  **single file with a larger edit**. Both exclude new files, schema/API/
  dependency/test changes, and cross-module refactors.
- **Merge eligibility** — the worker must be in review, every finding must be
  fixed or `wontfix` with a written justification, and every branch-tip check
  must have passed. A fix or fixer commit replaces the check result and must be
  re-run before merge.
- The lead is the reviewer of record. An independent reviewer is never a
  substitute PASS gate.

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
  mirroring the native-webview overlay rule.
- The team peek is anchored over the chat side.

## Next phases

See `.coding/design/mnemo2.md` for the full design and acceptance criteria:

1. **Lead engine + model registry** — complete: one real streaming lead session,
   config-backed model picker, project lead model.
2. **Parallel team runtime** — complete: shared engine, per-task
   branches/worktrees/briefs/models, worker messaging, slots, restart recovery.
3. **Review, merge, recovery, cost** — findings, tiny-work fixes, branch-tip
   checks, serialized local merge, conflict routing, budget enforcement.
4. **Native shell + workbench + persistence** — native WebView2 browser pane,
   sessions that survive restart, accessibility and narrow layouts. (The project
   registry/`projects.toml` and the folder picker landed early, in Phase 2.)
