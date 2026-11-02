# Plan: Files, Trace and Git tabs in the lead's side panel

## Goal
The lead's Workbench side panel gains Files (project tree + read-only viewer), Trace (live log of model requests with their full payloads) and Git (branches, recent commits, and moving main), next to Browser, Diff and Plan.

## Kind
implementation

## Context
Why: the user reviewed which Mnemo1 tools Mnemo2 still lacks; the lead proposed, and the user accepted, putting the "while talking to the lead" tools in the lead's side panel. Mnemo1 had them as right-panel views (`frontend/src/hooks/rightPanelViews.tsx`: Files → `FileViewer.tsx`, Trace → `LlmTraceView.tsx`, Git → `GitView.tsx`) backed by `src-tauri/src/ipc/files.rs` and `src-tauri/src/ipc/trace.rs`, which are Mnemo1-binary-only (Mnemo2 cannot call `src-tauri/src/ipc/*`; port the logic, do not import it). Trace matters: today a worker problem (an endpoint receiving strict-mode schemas) took log archaeology that a request inspector shows at a glance.

Verified facts:
- verified: Workbench tabs are a list in `mnemo2/src/components/Workbench.svelte` (`{ id: "browser" }, { id: "diff" }, { id: "plan" }`) with a body switch, and `WorkbenchTab = "browser" | "diff" | "plan"` in `mnemo2/src/lib/types.ts` line 8.
- verified: `Engine::trace()` (`src/headless/engine.rs` line 703) returns `TraceApi` (`src/headless/stats.rs` line 203) with `list()` → `Vec<LlmRequestSummary>` (216), `get(id)` → `Option<LlmRequestDetail>` (223), `clear()` (228) over the shared `LlmRequestLog`. Mnemo2 reaches the engine via `LeadRuntime::engine_handle()` (`src-tauri/src/mnemo2/runtime.rs` line 444).
- assumption: worker requests may be logged in per-worker engines rather than the lead's log; step 2 finds out and, if so, lists them too (with the worker named) or says plainly in the tab which requests it covers.
- verified: Mnemo1's git history shells out to `git` (`src-tauri/src/ipc/files.rs` lines 178-296, test `git_history_reports_branches_commits_and_spine` at 425); file reads go through `mnemo::tool::agent::sandbox::Sandbox` so paths stay inside the project.

Decisions:
- One new backend file per step: `src-tauri/src/mnemo2/files_view.rs`, `src-tauri/src/mnemo2/trace_view.rs`, `src-tauri/src/mnemo2/git_view.rs`, each registered in `src-tauri/src/mnemo2/mod.rs` directly after `ipc::workbench_preferences_set` in step order (files, then trace, then git), so later steps append below earlier ones.
- Each step appends its tab to the END of Workbench's tab list and the `WorkbenchTab` union, and its branch to the body switch; the steps run one after another because they share those lines.
- Files (step 1): lazy tree (expand directories on demand, .gitignore-aware, `.coding` and `.worktrees` collapsed), read-only viewer with syntax highlighting (reuse the Markdown/highlight setup), a path filter box; reads confined by the sandbox; large or binary files show a note instead of content.
- Trace (step 2): a request list (time, model, endpoint, tokens in/out, duration, status), polled while the tab is visible; selecting one shows the request (system prompt, messages, tool schemas, collapsible) and the response; a Clear button.
- Git (step 3): current branch, branches with ahead/behind main, the last ~50 commits as a simple lane list, and a "Move main here" action that fast-forwards main to the current branch only when it is a fast-forward (`git fetch . <branch>:main` semantics), never pushes, and asks for confirmation.
- Previews: one scene per step added directly after the `worker-thread` scene in `mnemo2/src/test/previewScenes.ts`, with dev-only seeds in each lib module (`npm run check:no-mock`).
- The plan "Lead side panel resizes freely" (c9b29308) also edits `Workbench.svelte`; this plan is dispatched after it merges.

Verification per step: `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::<module>`, `cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests --bins -- -D warnings`; in `mnemo2/`: `npm run check`, `npm run check:no-mock`, `npx vitest run`. Never start a dev server or any command that does not exit on its own; the lead renders previews.

## Acceptance scenarios
- The lead's side panel shows Browser, Diff, Plan, Files, Trace and Git tabs.
- Files lists the project tree lazily and shows a selected file read-only with highlighting; a path outside the project is refused.
- Trace lists recent model requests with model, tokens and duration, and selecting one shows the exact request and response.
- Git shows branches, recent commits and ahead/behind; Move main fast-forwards main after confirmation and refuses when it is not a fast-forward; nothing is pushed.

## Steps
- [x] 1. **Files tab** — new `src-tauri/src/mnemo2/files_view.rs` (+ tests incl. path escape refusal) registered in `src-tauri/src/mnemo2/mod.rs`, `mnemo2/src/lib/types.ts` (WorkbenchTab), `mnemo2/src/components/Workbench.svelte`, new `mnemo2/src/components/FilesTab.svelte` and `mnemo2/src/lib/files.ts` (+ tests), a `files` scene in `mnemo2/src/test/previewScenes.ts`.
- [x] 2. **Trace tab** — new `src-tauri/src/mnemo2/trace_view.rs` (+ tests) registered in `src-tauri/src/mnemo2/mod.rs`, `mnemo2/src/lib/types.ts`, `mnemo2/src/components/Workbench.svelte`, new `mnemo2/src/components/TraceTab.svelte` and `mnemo2/src/lib/trace.ts` (+ tests), a `trace` scene in `mnemo2/src/test/previewScenes.ts`.
- [x] 3. **Git tab** — new `src-tauri/src/mnemo2/git_view.rs` (+ tests incl. refusing a non-fast-forward move of main) registered in `src-tauri/src/mnemo2/mod.rs`, `mnemo2/src/lib/types.ts`, `mnemo2/src/components/Workbench.svelte`, new `mnemo2/src/components/GitTab.svelte` and `mnemo2/src/lib/git.ts` (+ tests), a `git` scene in `mnemo2/src/test/previewScenes.ts`.

## Interfaces
- 2026-11-02: Each step that adds a Workbench tab must also accept its tab id in `ProjectPreferences::validate` (src-tauri/src/mnemo2/project.rs, the allowed workbench_tab list) and in the `ProjectPreferences.workbench_tab` union in mnemo2/src/lib/projectEngine.ts, with a test that preferences carrying the new tab validate; otherwise selecting the tab makes every preference save fail.

## Progress
- 2026-11-02 step 1 done: step 1 merged: filesTab90 (filesTab90) on deepseek-v4.1-flash:1017, merge commit 404a8fb3; review: Files tab in the lead's side panel: lazy git-aware tree including staged files, read-only viewer confined to the project; preferences accept the files tab and window-relative widths up to 8192px; theme tokens only.
- 2026-11-02 step 2 done: step 2 merged: traceTab94 (traceTab94) on deepseek-v4.1-flash:1017, merge commit d04db570; review: Trace tab over the engine's shared LLM request log: summaries polled while visible, one detail fetched on demand with collapsed, capped payloads, and Clear; covers the lead and every worker because Mnemo2 runs one engine.
- 2026-11-02 step 3 done: step 3 merged: gitTab95 (gitTab95) on deepseek-v4.1-flash:1017, merge commit d6145946; review: Git tab: current branch, branches with ahead/behind main, recent commits, and a confirmed fast-forward-only Move main that never pushes.
