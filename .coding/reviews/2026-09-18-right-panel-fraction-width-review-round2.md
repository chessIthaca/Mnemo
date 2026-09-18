## Verdict: PASS

Round-2 verification of plan 38b0e2eb ("Right-panel width as a window fraction") on `wt/mnemo` — the full uncommitted diff (7 modified files + the plan file, the round-1 report, and `RightPanel.width.test.tsx` untracked) against round-1's single LOW finding (L1: render-time-only band clamp). **L1 is genuinely closed**: the band caps now ride inline in the width style — `min(pct%, 50%, calc(100% - 480px))` — so the CSS layout engine re-clamps at every viewport with no listener and no re-render; the constants are truly shared with `clampPanelFraction`; the updated regression test guards both the original bug and the inline caps; and the round-1-verified surface is otherwise untouched. No new findings.

## Verified correct

**1. The L1 scenario is closed (the headline check).**
- `RightPanel.tsx:43-51` renders `width: min(${pct}%, ${PANEL_MAX_FRAC*100}%, calc(100% - ${CHAT_MIN_PX}px))`. Exact round-1 scenario replayed by hand: stored frac 0.5 (legal on a ≥960px window, where the band max = min(0.5, (w−480)/w) = 0.5) renders `min(50%, 50%, calc(100% - 480px))`. Shrink the window to 800px with **no re-render**: the style string is unchanged, and CSS resolves it against the new viewport — 50% → 400px, calc(100% − 480px) → 320px, `min()` picks **320px** — exactly the band ceiling at 800px ((800−480)/800 = 0.4). Chat column = 800 − 56 (sidebar) − 6 (handle) − 320 = 418px, the documented "~480px measured from the window" floor. Continuous, no listener, no stale clamp. ✓
- **Percentage base confirmed**: the RightPanel root div is a direct child of App's root (`flex h-screen w-screen`, App.tsx:685 — no padding; ResizeHandle + RightPanel render as direct children, App.tsx:796-805), so `100%` in both the bare percentage and the `calc()` resolves against the full window width. The `w-14` sidebar and `w-1.5` handle are flex *siblings*, not part of the percentage base. ✓
- **The floor edge is covered between renders too**: a shrink can drop the bare pct below 300px (e.g. frac 0.3 at 1000 → 240px at 800); the retained `min-w-[300px]` class (RightPanel.tsx:42) catches it (CSS min-width wins over width). So between renders the ceiling is enforced inline and the floor by class — both band edges hold. ✓
- On the next render (tab toggle, drag, hide/show) `clampedFrac` recomputes against the current viewport, so even the rendered *percentage* tightens (0.5 → 0.4 at 800) — the stale-frac window is bounded by the inline caps the whole time. ✓

**2. The constants are genuinely shared — no drift.**
- `appearance.ts:243/248` exports `CHAT_MIN_PX = 480` and `PANEL_MAX_FRAC = 0.5`; `clampPanelFraction` (:259-264) computes max = `min(PANEL_MAX_FRAC, (w − CHAT_MIN_PX)/w)`; `RightPanel.tsx:6` imports the *same two constants* and interpolates them into the style (:46-48) → `50%` and `calc(100% - 480px)`. The inline caps are literally the JS band's max expressed in CSS — one source of truth. `PANEL_MAX_FRAC * 100` = 50 exactly (no float artifact). ✓
- Consistency with the drag math: `ResizeHandle`'s `startWidth = clampPanelFraction(frac, innerWidth) × innerWidth` (App.tsx:861) equals what the inline `min()` renders, so no jump at pointerdown. ✓

**3. The regression test guards both the original bug and the new caps.**
- Test 1 (legacy 831px @ 800, real component through the real store): renders `min(40%, 50%, calc(100% - 480px))` → passes `toMatch(/%/)` (✓ contains %), `not.toMatch(/px$/)` (✓ ends `"))`, not px), `toContain("50%")`, `toContain("calc(100% - 480px)")`. ✓
- **Old code** rendered `width: 640px` → fails `toMatch(/%/)` → the original 80%-cap bug stays guarded. The round-1 intermediate state (plain `40%`) → fails `toContain("50%")` → the inline caps are now guarded. Both generations of the defect are pinned. ✓
- Test 2 (percentage-ness at 1000/1400) unchanged — the correct resize-tracking contract (the *string* stays a percentage; asserting a string change would be wrong). ✓
- Registered in the vitest include list (vitest.config.ts:87). ✓

**4. No regression in the round-1-verified surface.**
- The diff outside RightPanel.tsx/the constants' doc comments/the test assertions is byte-identical to what round 1 verified: drag math (live `window.innerWidth` per `fracFromPx` call, live-vs-commit store paths, pointer-capture/cancel/unmount cleanup), legacy seed (`readRightPanelWidthFrac`: frac-key precedence, `typeof window` guard *before* the `window.innerWidth` division, no migration write), store rename (search `rightPanelWidth[^F]` over `frontend/**` finds only the legacy LS-key constant, its doc, and test fixtures — all intentional), SSR/node-env guards, and the four `clampPanelFraction` + three `readRightPanelWidthFrac` unit cases (hand-checked: (0.4,1000)→0.4; (831/800,800)→0.4; (0.9,2000)→0.5; (0.05,1000)→0.3). ✓
- No resize listener was added anywhere (the accepted no-listener design stands; App's `onResized` still only persists geometry). ✓

**5. Multi-arg CSS `min()` support — both webviews fine.**
- MDN browser-compat-data (`css/types/min.json`, fetched this session): `min()` — Chrome 79+, Safari 11.1+, Firefox 75+. WebView2 is evergreen Chromium (≥ 79 by construction); WKWebView is the macOS Safari engine (≥ 11.1, macOS 10.13+). Multi-argument is the spec form (comma-separated list, css-values-4). Both targets covered with years of margin. ✓

**Constitution extras**
- **Docs sync** ✓ — the RightPanel render comment (:28-34) and both constants' doc comments now describe the inline caps accurately; the test file's header comment updated to match; the "never" promise the round-1 finding quoted is now actually true at every viewport. No stale docs elsewhere (round-1 checked FEATURES/PLAN.md; nothing here touches them).
- **Multi-platform neutrality** ✓ — pure CSS + web APIs; no platform-specific code.
- **File-tools-first** ✓ — no shell-based mutation in the diff; the `.coding/backlog.jsonl` addition is app-managed bookkeeping (an unrelated pending backlog item).
- **Security** ✓ — the style value is numerically derived from a clamped fraction; no injection surface.
- **Plan context** ✓ — the plan file documents the L1 fix and the re-verification (38b0e2eb.md:12).
- Tests: the claimed green runs (full vitest exit 0, tsc + vite build clean, cargo test 2410 passed / 0 failed / 5 ignored + 16 integration) are consistent with the code as verified; not re-runnable by this read-only reviewer.

## Notes (no action required)

- The 300px floor literal exists in two places — `clampPanelFraction`'s `300 / w` and the `min-w-[300px]` class. Pre-existing from round 1 (the class was noted there), unchanged by this fix; the two agree at every reachable viewport.
- The 0.1%-rounded percentage can round a stored frac slightly above its band max (≤ 0.05%), but the inline `calc(100% - 480px)` cap re-clamps it — the rounding is now harmless by construction (round-1 note 44's residual is subsumed by the inline caps).
- Degenerate `calc(100% - 480px) < 0` (window < 480px) is unreachable — the OS-enforced 800px minimum plus the startup geometry clamp.
- The persisted frac can stay stale-high (e.g. 0.5) across a shrink until the next drag; the inline caps bound the rendered width the whole time, and the next render tightens the percentage. Sound within the no-listener design.
