+++
title = "verify Svelte assignment_value_stale warnings (dev-runtime only)"
created = "2026-11-05"
+++

Svelte 5's `assignment_value_stale` is a **dev-runtime client** warning (node_modules/svelte/src/internal/client/dev/assign.js), emitted only from the `$.assign(...)` dev helper: it fires when the assignment's value differs from the property's value after assignment AND that stored value is an object carrying STATE_SYMBOL (e.g. `obj.field = { ...stateProxy }`). Verified on Svelte 5.57.3:
- `vite build` can never print it — production compiles plain assignments — so a clean build is not evidence the warning is gone.
- Block-bodied and expression-bodied arrow callbacks (`() => (a = b)` vs `() => { a = b; }`) compile to the same assignment; changing the arrow body cannot silence this warning.
- Warning text points at the assignment location; the real remedy for a state-proxy RHS is assigning a plain snapshot (or untracking the read), not re-wrapping the arrow body.
Corollary for verification: to red-check such a warning you need a dev-mode runtime (dev server/preview scene or a vitest run whose Svelte compile is dev), and a control that proves dev mode is actually on — a build or a silent test run proves nothing either way.
