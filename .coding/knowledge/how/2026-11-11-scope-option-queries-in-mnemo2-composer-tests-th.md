+++
title = "scope option queries in mnemo2 composer tests — the model select also renders options"
created = "2026-11-11"
evidence = "Composer.test.ts test \"suggests commands while the name is typed and runs the highlighted one\" failed with 3 options before adding within(); passes after (npm test --workspace mnemo2, 1423 passed)."
+++

`screen.getAllByRole("option")` in a mnemo2 component test also matches the model-picker `<option>`s: LeadModelPicker/ModelEffortPicker render a `<select>` with one option per `$app.leadRuntime.models` entry, so a slash-menu assertion expecting N options sees N + models.length. Fix: scope the query — `within(screen.getByRole("listbox", { name: "Slash commands" })).getAllByRole("option")` (import `within` from `@testing-library/svelte`). Found while writing Composer.test.ts slash-menu tests (commit cffc598f).
