+++
title = "Mnemo2 plan-list state lives in PlanTab.svelte, not PlansView.svelte"
created = "2026-11-02"
+++

Mnemo2's plans UI is split so that `PlansView.svelte` is only a thin wrapper (25 lines: header + `.plans-scroll` holding `BacklogSection` then `<PlanTab />`). All plan-list state — `expanded`, `details`, `loadDetails`, `toggleDetails`, the `planCard` snippet — lives in `PlanTab.svelte`, whose `scope` prop defaults to `"all"` (the Plans route) and is passed `"open"` by `Workbench.svelte` for its workbench plan tab. So any per-plan interaction (expand, load a document, scroll a card into view) must be implemented in `PlanTab.svelte`, and a hand-over meant for the Plans route should guard on `scope === "all"` so the workbench's `open`-scope copy does not consume it. Card elements carry `data-plan-id` for a container query; `PlansView` needs no change for such features.
