+++
title = "plan-steps UI — headline + collapsible details; headline-only executing popup"
created = "2027-01-04"
status = "superseded"
+++

Plan-steps display (plan 8ba00d97, backlog af572504, commit ede22cb, branch wt/agenticcoding):

1. **Plan window (PlanProgress.tsx)**: step rows are headline + collapsible details via the pure `PlanStepRow` component (frontend/src/components/views/PlanStepRow.tsx). Headered steps (`**Header** — body`): bold headline always visible, rotating-chevron button (aria-expanded) reveals `stepBody(text)`; default collapsed. Plain headerless steps render the whole text (stepBody passes them through — nothing to reveal, no chevron); header-only steps get an alignment spacer. The ACTIVE step (first not-done, while state === "executing") auto-expands and auto-collapses on completion; manual toggles record overrides (`expandOverrides[step.index] ?? step.index === activeIndex`). Resets are signature-keyed (title + step count) — cover create_plan replacement and update_plan append/trim; same-title-same-length replacement is the one documented cosmetic residual. The read-only ancestor view uses the same rows, collapsed by default, reset per ancestor switch.

2. **Executing popup (StatusBar.tsx → ExecutingStepPopup.tsx)**: shows ONLY the current step's headline — single CSS-truncated line (`truncate`), never the body; headerless steps show the first non-blank line (`stepHeadline` in frontend/src/lib/planSteps.ts); "All steps complete" when done. The `step x/y` number derives from `current.index + 1` (exact; the toolbar's stateLabel agrees because done steps are always a prefix). The full headline rides the title attr.

3. **Data model untouched**: the stored `**Header** — body` format, the plan file, the model's plan rider, and the IPC payload are unchanged — display-layer only.

Tests: PlanStepRow.test.tsx + ExecutingStepPopup.test.tsx (renderToStaticMarkup markup pins) + PlanProgress.expand.test.ts (source contracts) — registered in frontend/vitest.config.ts's include allowlist (new frontend test files MUST be added there; it is not a glob).
