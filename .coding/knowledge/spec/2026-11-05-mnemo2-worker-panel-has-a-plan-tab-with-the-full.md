+++
title = "Mnemo2 worker panel has a Plan tab with the full plan and the current specs it changes"
created = "2026-11-05"
+++

mnemo2/src/components/WorkerDetail.svelte shows a fourth tab, Plan, for any worker with a planId. mnemo2/src/components/WorkerPlanTab.svelte renders the whole plan document (read via createBoardTransport().document, re-read on board changes while keeping the shown text, so scroll and open folds survive), a header 'Plan <id> · step N' (planStep is 0-based) and an 'Open in Plans' button (actions.openPlanDocument). For each capability named under the plan's '## Spec changes' (lib/planDocument.ts specCapabilities: kebab-case '### <name>' headings) it shows a folded 'Current spec: <name>' that reads the spec once on first open through the Tauri command read_capability_spec({ capability }) in src-tauri/src/mnemo2/specs_settings.rs, which returns Option<String> for .coding/openspec/specs/<capability>/spec.md, refuses non-kebab names and linked folders/files.
