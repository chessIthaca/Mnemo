+++
title = "Mnemo2's lead side panels resize up to the window minus a 420px lead"
created = "2026-11-02"
status = "superseded"
+++

Both of the lead's side panels (Workbench and the worker output panel) share app.workbenchWidth, clamped by clampWorkbenchWidth in mnemo2/src/lib/store.ts to [MIN_WORKBENCH_WIDTH 320, window.innerWidth − MIN_LEAD_WIDTH 420] and re-clamped on mount and window resize (reclampWorkbenchWidth), so a saved wide panel never squeezes the lead. LeadChat's .lead-workspace is a size container; under 600px its gutters drop from 32px to 16px. The Workbench's Plan tab renders PlanTab scope="open" (open plans plus an "All plans" link to the Plans route); the Plans route renders scope="all".
