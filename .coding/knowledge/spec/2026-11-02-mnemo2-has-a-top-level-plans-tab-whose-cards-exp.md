+++
title = "Mnemo2 has a top-level Plans tab whose cards expand to the full plan"
created = "2026-11-02"
+++

Mnemo2's top bar has Lead, Team, Cost, Graph and Plans (route "plans", actions.openPlans, PlansView.svelte reusing PlanTab.svelte cards). Each plan card keeps its compact look and has a "Details" toggle that lazily loads the plan file through the Tauri command board_plan_document(plan_id) (src-tauri/src/mnemo2/ipc.rs → TeamRuntime::plan_document → PlanBoard::read_markdown, which refuses ids without a board.json row) and renders it as Markdown minus the title and ## Goal (mnemo2/src/lib/planDocument.ts). board_snapshot stays small (no context/acceptance/decisions); open cards re-fetch on mnemo2://board-changed. The Workbench's Plan tab shares the same cards. Preview scene: plans (seed seam seedBoardPreview in lib/boardEngine.ts).

Amended 2026-11-09: The top bar now lists Lead, Team, Cost and Plans: Graph, Trace and Memory became side-panel tabs. Plans stays a main tab, and the panel can stay open beside it.
