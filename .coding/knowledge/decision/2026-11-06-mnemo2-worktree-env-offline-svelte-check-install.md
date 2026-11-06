+++
title = "mnemo2 worktree env (offline svelte-check install, WMI-detached dev server) + divider/a11y conventions"
created = "2026-11-06"
+++

Durable facts for mnemo2 frontend work:

1. ENVIRONMENT: Mnemo2 worker worktrees (C:\mnemo\.worktrees\agent-*) ship no real node_modules — vitest/svelte resolve by walking up to the MAIN checkout's hoisted C:\mnemo\node_modules, which has vitest but NOT svelte-check, so `npm run check --workspace mnemo2` fails with "'svelte-check' is not recognized" and there is no network access. Fix: `npm install --workspace mnemo2 --prefer-offline --no-audit --no-fund` from the worktree root installs ~159 packages offline from the warm npm cache in ~20s. Afterwards `git restore package-lock.json` — npm 11.5.1 rewrites the lockfile (prunes optional platform entries) and that must not ship. To keep a Vite dev server alive across shell calls, spawn it via `Invoke-CimMethod Win32_Process Create` (WMI) — the shell tool kills its own child process tree at call end (Start-Process children die too); WMI children survive. mnemo2 dev server: port 5180, preview scenes at preview.html?scene=<name> (trace scene seeds the Trace tab).

2. DECISION (lead-approved 2026-11-06): Svelte's a11y checker forbids role="separator" on any keyboard-accessible element — <button role="separator"> trips a11y_no_interactive_element_to_noninteractive_role, <div role="separator" tabindex="0"> trips a11y_no_noninteractive_tabindex + a11y_no_noninteractive_element_interactions — and the 0-warning svelte-check gate is the firmer requirement. The repo's divider convention (TraceView vertical divider, ControlRoom pane-divider) is a plain <button> with aria-label + title; new splitters must follow it, not the ARIA APG separator pattern.

3. BUG pattern: element.scrollHeight is NOT the natural content height of a fixed-height box — it returns max(box height, content height), so measuring "natural" via scrollHeight silently breaks exactly when the fixed height exceeds the content (the grow direction). Measure the content element's own offsetHeight instead (e.g. `(body.firstElementChild as HTMLElement | null)?.offsetHeight`).
