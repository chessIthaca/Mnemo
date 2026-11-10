+++
title = "Mnemo2's side panel is on every main tab and hosts Trace, Graph and Memory"
supersedes = "2026-11-02-mnemo2-s-lead-side-panels-resize-up-to-the-windo"
created = "2026-11-09"
evidence = "App.navigation.test.ts 'the side panel on every main tab'; Workbench.test.ts 'opens the wide tabs wider and remembers each width apart'; project.rs preferences_validate_the_wide_panel_width; rendered at 420 to 1920 wide; the user asked for it on 2026-11-09."
+++

The Workbench (mnemo2/src/components/Workbench.svelte) renders at app level beside the main tab (App.svelte `.main-column` holds `.main-route` and the panel), so main routes are only lead, team, cost and plans. Strip order: Browser, Diff, Plan, Files, Git, Trace, Graph, Memory. The stored tab is validated natively: WORKBENCH_TABS in src-tauri/src/mnemo2/project.rs must match the WorkbenchTab union, or every preference save fails.

Open state and tab are independent of the route: opening the panel never changes the route and it stays open across main-tab switches, except at window width <=980px, where it covers the main tab, so navigating closes it (panelAfterNavigation in lib/store/navigation.ts). The top-bar button shows on every tab and reopens the last tab. WorkerPanel stays Lead-only and shares the slot; opening the Workbench clears it. PlanTab's "All plans" closes the panel, because its open-plan list beside the full list is a duplicate. Plans stayed a main tab because its documents read best at full page. The native Browser pane is allowed on any route except start.

Widths (lib/store/layout.ts): workbenchWidth for Browser, Diff, Plan, Files, Git and WorkerPanel; Trace, Graph and Memory are wide tabs with their own workbenchWideWidth, persisted as workbench_wide_width, null following the window at 60%. 0.6 is chosen so Trace keeps two columns at a 1280 window and Mission Control keeps two columns at 1920. Both clamp to [320, window - MIN_LEAD_WIDTH 420] and re-clamp on mount and window resize; double-clicking the resizer resets a wide width. No setting exists by design.

A main tab can no longer read its width from the window, so views use container queries or measure themselves: ControlRoom stacks at <=760px of its own, PlansView at <=640px, MemoryView narrows at 820px and stacks at 580px, GraphView drops to two columns at 880px and stacks at 520px, TraceView goes single-column under 760px. Saved inner widths yield through CSS (`minmax(min, saved)` grid columns, a `max-width` on the backlog). At <=760px the top bar drops backdrop-filter, because it would make the bar the containing block of the fixed bottom nav and park the nav over the panel button. LeadChat's .lead-workspace is a size container whose gutters drop from 32px to 16px under 600px.
