+++
title = "Mnemo2 Plans page has white plan cards and a resizable backlog column with done checkboxes"
created = "2026-11-05"
+++

The Plans route (mnemo2/src/components/PlansView.svelte) is two columns: plans on a gray column (var(--surface-soft)) as white bordered cards with 14px gaps (PlanTab.svelte .plan; the Workbench open-plan tab shares the style), and the backlog in a white right column split by a draggable/keyboard splitter (280px to 60% of the page, default 380, saved per viewer in localStorage mnemo2.plans.backlogWidth; stacked under 760px). BacklogSection.svelte shows collapsed rows (checkbox, expand chevron, first line, badges) in three groups: the queue, "With the lead" (in_flight, checkbox disabled) and a collapsible "Done" group holding Clear finished. The checkbox calls the backlog_set_done Tauri command (src-tauri/src/mnemo2/backlog.rs): checked uses the store's guarded transition to Done, unchecked uses requeue back to Pending; an in-flight idea is refused.
