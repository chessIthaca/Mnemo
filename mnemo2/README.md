# Mnemo2 — Phase 0b prototype

A standalone Svelte 5 workspace for the Mnemo2 **lead + team** cockpit. This
phase is clickable but **mock-only**: it does not touch the Mnemo engine, Tauri
IPC, or git. It validates the conversation-first interaction model before the
native shell is built: the lead owns a workflow for each task, tasks run in
parallel, and the lead and worker models are visible in the UI.

## Run

From the repository root:

```bash
npm install
npm run dev:mnemo2      # http://127.0.0.1:5180
npm run check:mnemo2    # svelte-check (0 errors / 0 warnings expected)
npm run test:mnemo2     # Vitest interaction + policy tests
npm run build:mnemo2    # -> target/mnemo2-dist
```

`mnemo2` is a separate npm workspace (`mnemo2/`). Vite uses port **5180** and
outputs to `target/mnemo2-dist`, distinct from the Mnemo1 frontend on 5179.

## Prototype tour

1. Pick **Mnemo** on the start screen.
2. Send a message to the lead. Lead responses render sanitized Markdown with
   tables and code. Delegation and status appear as compact task cards; there
   is no global run diagram.
3. Open **Team** for the ten-worker task board. Each row shows its model and
   its own workflow state; tasks advance independently. Filter **Needs you /
   Active / Done**, or change the per-project **worker slots** setting. The
   board is deliberately designed for larger teams (8+), not a fixed three.
4. Open **Ana**. The conversation opens first; **Review** holds her checks and
   findings. Her two findings block merge. **Fix tiny** applies the lead's
   `review_fix` path; **Spawn fixer** routes the larger finding. Apply the mock
   fixer result, then **Merge reviewed branch**. The merge writes a record into
   the lead chat.
5. Open the workbench from the top bar, then switch to **Diff** — the lead's
   reviewed-branch stand-in. Use **Browser / Diff / Plan**; the pane is
   resizable on desktop and full-screen on narrow layouts.
6. Open **Project settings** from the budget chip to change worker slots, see
   the lead model, or replay the prototype.

## What is mocked

- **Workers, checks, costs, diffs, workflows, and model labels** come from
  `src/lib/mock.ts`. Ten workers cover dispatch, work, review, fixing, merging,
  merged, conflict, failed, and cancelled states. Their per-task workflows and
  model badges are mock data; Phase 1 binds real models and Phase 2 binds the
  live runtime.
- **Parallel tasks are simulated.** Each worker advances independently, so the
  UI can show different tasks in different workflow states at once; there is no
  global run timeline.
- **No git operations.** The merge is a store transition; it never writes a
  branch, commit, or worktree.
- **No Engine or IPC.** There is no Tauri `invoke`, no `mnemo::headless` import,
  and no browser child webview. The Browser tab is a labelled stand-in —
  Phase 4 embeds the native WebView2 pane, because an iframe is not a browser.
- **No persistence.** Reloading resets the demo; **Replay prototype** in
  project settings returns to the start screen.

## Policy encoded in the prototype

The mock still enforces the product rules, in `src/lib/review.ts`:

- **Lead tiny-work cap** — either up to **5 files × 10 changed lines**, or a
  **single file with a larger edit**. Both exclude new files, schema/API/
  dependency/test changes, and cross-module refactors.
- **Merge eligibility** — the worker must be in review, every finding must be
  fixed or `wontfix` with a written justification, and every branch-tip check
  must have passed. A fix or fixer commit replaces the check result and must
  be re-run before merge.
- The lead is the reviewer of record. An independent reviewer is never a
  substitute PASS gate.

## Layout notes

- Warm-white tokens, Inter UI, Source Serif 4 prose; prefers-reduced-motion is
  respected.
- Conversation-first: the lead chat and worker **Thread** are primary; review,
  checks, timeline, and merge are secondary disclosures.
- No project-wide run pipeline or phase diagram. Each task derives its own
  workflow from status, checks, findings, and merge state.
- Every lead/worker surface shows a model badge; the lead model also appears in
  project settings.
- Workbench lives beside the lead chat only; opening Mission Control hides it,
  mirroring the native-webview overlay rule.
- The team peek is anchored over the chat side.

## Next phases

See `.coding/design/mnemo2.md` for the full design and acceptance criteria:

1. **Lead engine + model registry** — one real streaming lead session, config-
   backed model picker, project lead model.
2. **Parallel team runtime** — TeamManager, per-task briefs/workflows,
   worktrees, worker messaging, configurable slots, restart recovery.
3. **Review, merge, recovery, cost** — findings, tiny-work fixes, branch-tip
   checks, serialized local merge, conflict routing, budget enforcement.
4. **Native shell + workbench + persistence** — Tauri/WebView2, project
   registry, sessions that survive restart, accessibility and narrow layouts.
