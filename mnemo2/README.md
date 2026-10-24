# Mnemo2 — Phase 1 native lead

A standalone Svelte 5 workspace for the Mnemo2 **lead + team** cockpit. In the
native Tauri host the lead conversation is real: it runs one headless-engine
session with streaming events, cancellation, transcript restore, approvals, and
a config-backed model picker. The browser-only Vite session remains a mock
fallback so the conversation-first UI stays clickable without a native shell.

The product shape is unchanged from Phase 0b: the lead owns a workflow for each
task, tasks are independent, and the lead and worker models are visible wherever
an agent appears. Parallel workers and the team runtime are still Phase 2.

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
- **Interaction.** The composer sends through the real session when native and
  falls back to the mock reply in browser mode. While a turn runs, the send
  button becomes a stop button; `lead_interrupt` cancels the generation without
  closing the session. Errors render inline with **Retry**; pending approvals
  and questions render inline with their actions.
- **Restore.** The transcript is persisted when a turn finishes or parks and is
  resumed on the next native open.
- **Known Phase 1 limitation.** Native open still uses the Phase 0b seeded
  project paths. If the selected seed path does not exist on this machine (for
  example a Windows seed opened on macOS), the native lead reports the runtime's
  inline "project root does not exist" error; the native project registry and
  folder picker are Phase 4. The browser fallback never touches the filesystem.

## Prototype tour

1. Pick **Mnemo** on the start screen.
2. In the native host, send a message to the lead. Lead responses stream into
   the conversation and render sanitized Markdown with tables and code. In
   browser mode the same surface shows the mock reply.
3. Choose the lead model from the composer or **Project settings**. The picker
   is disabled while a turn is running.
4. If the lead asks for approval or asks a question, resolve it inline. Use the
   stop button to cancel a running generation.
5. Open **Team** for the ten-worker mock task board. Each row shows its model
   and its own workflow state; tasks advance independently. Filter **Needs you /
   Active / Done**, or change per-project **worker slots**.
6. Open **Ana**. The conversation opens first; **Review** holds her checks and
   findings. The review/merge flow is still mock policy: fix or `wontfix` every
   finding, then merge the reviewed branch into the lead chat record.
7. Open the workbench from the top bar, then switch to **Diff**. Use **Browser /
   Diff / Plan**; the pane is resizable on desktop and full-screen on narrow
   layouts.

## What is real vs. mocked

- **Real (native host):** lead session, streaming, model identity and picker,
  cancellation, transcript persistence/restore, approvals/questions.
- **Mock:** workers, checks, costs, diffs, workflow states, worker branches,
  review findings, and merge records. Ten workers cover dispatch, work, review,
  fixing, merging, merged, conflict, failed, and cancelled states. Parallel
  tasks are simulated; there is no global run timeline.
- **No git operations.** The merge is a store transition; it never writes a
  branch, commit, or worktree.
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
2. **Parallel team runtime** — TeamManager, per-task briefs/workflows,
   worktrees, worker messaging, configurable slots, restart recovery.
3. **Review, merge, recovery, cost** — findings, tiny-work fixes, branch-tip
   checks, serialized local merge, conflict routing, budget enforcement.
4. **Native shell + workbench + persistence** — project registry, native
   WebView2 browser pane, sessions that survive restart, accessibility and
   narrow layouts.
