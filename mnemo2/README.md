# Mnemo2 — Phase 0 prototype

A standalone Svelte 5 workspace for the Mnemo2 **lead + team** cockpit. This
phase is clickable but **mock-only**: it does not touch the Mnemo engine, Tauri
IPC, or git. It exists to validate the interaction model before the native
shell is built.

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
2. Send a message to the lead. Expand **Run 04** for the connected activity
   steps; lead responses render sanitized Markdown with tables and code.
3. Open **Team** for the ten-worker board. Filter **Needs you / Active /
   Done**, or change the per-project **worker slots** setting. The board is
   deliberately designed for larger teams (8+), not a fixed three.
4. Open **Ana**. Her two findings block merge. **Fix tiny** applies the
   lead's `review_fix` path; **Spawn fixer** routes the larger finding.
   Apply the mock fixer result, then **Merge reviewed branch**. The merge
   writes a record into the lead chat.
5. Open the workbench from the top bar, then switch to **Diff** — the lead's
   reviewed-branch stand-in. Use **Browser / Diff / Plan**; the pane is
   resizable on desktop and full-screen on narrow layouts.
6. Open **Project settings** from the budget chip to change worker slots or
   replay the prototype.

## What is mocked

- **Workers, checks, costs, and diffs** come from `src/lib/mock.ts`. Ten
  workers cover dispatch, work, review, fixing, merging, merged, conflict,
  failed, and cancelled states.
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
- Workbench lives beside the lead chat only; opening Mission Control hides it,
  mirroring the native-webview overlay rule.
- The team peek is anchored over the chat side.

## Next phases

See `.coding/design/mnemo2.md` for the full design: Phase 1 wires the engine,
TeamManager, worktrees, and serialized auto-merge; Phase 2 adds messaging,
worker chat, tiny-work policy, and cost telemetry; Phase 3 covers conflicts,
revert, and budget enforcement; Phase 4 lands the native browser, settings,
persistence, and recovery polish.
