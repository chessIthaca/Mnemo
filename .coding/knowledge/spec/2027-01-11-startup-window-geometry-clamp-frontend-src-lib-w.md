+++
title = "Startup window-geometry clamp (frontend/src/lib/windowRestore.ts)"
created = "2027-01-11"
+++

Landed 2027-01-14, plan 9ff59133 (branch wt/mnemo), user report: "when starting mnemo it remember the size it was at ... the app start for this one was a tiny small window top left on the screen".

Architecture: the mount restore in frontend/src/App.tsx delegates the whole geometry decision to the pure module frontend/src/lib/windowRestore.ts → `clampRestoredGeometry({saved, factor, monitors, chrome, minOverlapPx?})` → `{width, height, x: number|null, y: number|null}` (logical px). Unit-tested by frontend/src/lib/windowRestore.test.ts (23 cases: helper math + App.tsx source contracts via `?raw`), registered in frontend/vitest.config.ts's explicit include allow-list (a new test file is SILENTLY skipped there otherwise).

Invariants to preserve:
1. **Startup-only** — nothing in the helper may constrain interactive resizing; the Rust `min_inner_size(800,560)` stays authoritative there (see the min_inner_size SPEC). The helper is called exactly once, from the mount restore.
2. **Unit convention (load-bearing)** — the save path stores OUTER bounds (`outerSize()`, logical px) while `setSize` applies an INNER (client) size (tao has no outer-size setter; `set_inner_size` expands to the frame). The restore measures `chrome = outerSize() − innerSize()` and subtracts it, else the window GROWS by the chrome on every restart. The minimum is applied in INNER units — no double subtraction.
3. **Anchor split** — sizing anchor = the monitor with the largest positive overlap (best overlap overall as the fallback when none clears the gate); position anchor must clear the usability gate `MIN_VISIBLE_PX` = 80 LOGICAL px, compared as 80 × factor physical (parity with the pre-clamp gate). Below the gate the clamped window is CENTERED in the sizing anchor's work area; `x`/`y` are null only when NO monitor is known (caller then keeps the OS default).
4. **Per-probe degradation** — factor → 1 (also guards a resolved 0/NaN), monitors → [] (size bounded by the minimum ONLY, never shrunk), chrome → {0,0}.
5. **Per-monitor scaleFactor is deliberately not consulted** — the saved rect uses the window's factor (the pre-clamp approximation), pinned by the mixed-DPI test case.
6. Fully visible geometries pass through unchanged; the minimum floor wins when the work area is smaller than the min (the title bar then sits at the work-area corner).

The mirrored constants `RESTORE_MIN_WIDTH`/`RESTORE_MIN_HEIGHT` (800/560) are prose-only mirrors of src-tauri/src/main.rs `min_inner_size` (a code pin would need a cross-root `?raw` import + vite fs.allow).
