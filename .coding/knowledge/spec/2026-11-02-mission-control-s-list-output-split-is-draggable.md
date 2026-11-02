+++
title = "Mission control's list/output split is draggable and remembered per project"
created = "2026-11-02"
+++

Mission control (ControlRoom.svelte) has a focusable divider between the worker list and the worker output: drag or ArrowLeft/ArrowRight (±24px) resizes, double-click resets to the default proportions (0.88fr / 1.12fr when no width is saved). The list keeps at least 300px (MIN_BOARD_WIDTH, room for the 152px status column) and the output at least 420px (MIN_BOARD_OUTPUT_WIDTH); reclampBoardWidth runs on mount and window resize. The width is saved per project as ProjectPreferences.board_width (Option<u32>, serde default, validated like workbench_width in src-tauri/src/mnemo2/project.rs; board_width in lib/projectEngine.ts and AppState.boardWidth). The stacked layout under 760px has no divider.
