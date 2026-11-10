+++
title = "Mnemo2 Plans tab — top-level route with expandable plan details"
created = "2026-11-02"
+++

Mnemo2's Plans tab is a top-level route beside Lead/Team/Cost/Graph, and its plan cards expand to the whole plan document.

- `mnemo2/src/components/PlansView.svelte` renders the existing `PlanTab.svelte` full-width; the Workbench keeps its narrow Plan tab. The card look is unchanged when closed; a quiet header "Details" control (aria-expanded, chevron rotates) opens the document.
- Details read lazily through `BoardTransport.document(planId)` → Tauri command `board_plan_document(plan_id: String)` in `src-tauri/src/mnemo2/ipc.rs`, backed by `TeamRuntime::plan_document` (`pub(crate)`). One fetch per successful load; open cards re-read on `mnemo2://board-changed`; loading and error lines are shown. `board_snapshot` carries no new fields.
- The command refuses an id that no board row names before touching any file (`PlanBoard::read_markdown` calls `load(id)` first), so a stray `.coding/plans/<id>.md` can never leak through IPC.
- `mnemo2/src/lib/planDocument.ts::planDetailMarkdown(markdown, keepSteps)` drops the leading `# Plan:` h1 and the `## Goal` section (already on the card) and drops `## Steps` unless at least one step shows a header; fenced code is fence-tracked, so `## ` inside code is content.
- Preview: `seedBoardPreview(transport)` in `mnemo2/src/lib/boardEngine.ts` is a dev-only seam consulted by `createBoardTransport` only without a Tauri runtime; the `plans` preview scene mounts the real App and clicks the first `.details-toggle` (the settings-laya scene uses the same mount-then-click pattern).
- `PlanBoard::create` validates plan thickness (context >= ~40 chars with paths + verification command, every step naming a file path), so Rust test fixtures that build plans must satisfy it.

Amended 2026-11-09: The main tabs beside Plans are now Lead, Team and Cost; Graph moved into the side panel.
