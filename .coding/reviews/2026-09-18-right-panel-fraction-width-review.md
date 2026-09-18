## Verdict: FINDINGS (0 high, 1 low)

Review of plan 38b0e2eb's uncommitted changes on `wt/mnemo` (7 modified files + 2 untracked: the plan file and `RightPanel.width.test.tsx`). The fix is substantively correct: the fraction math, band edges, drag px↔frac conversion, legacy seed, and store rename all check out against the real layout, and the regression test genuinely exercises the changed render path (fails as "640px" against the old code, passes as "40%" with the fix). One LOW residual: the band clamp is render-time-only, so a window shrink below ~960px can leave the panel at up to 50% (band-violating) until the panel next re-renders — a bounded edge of the accepted no-listener design, not a reintroduction of the reported bug.

## Verified correct

**Fraction math + band edges** (`clampPanelFraction`, appearance.ts:246-251) — hand-checked at every edge:
- w=800: band [0.375, 0.4] → legacy 831px seeds to 0.4 → 320px panel + ~424px chat. The reported bug (640px panel / ~160px chat sliver) is fixed.
- w=960: max = 0.5 exactly (chat-min 480 = half). w=480: band collapses to [0.625, 0.625] → 300px panel (floor wins over chat-min — correct priority, degenerate but safe). w=400: [0.75, 0.75] → 300px, no NaN, no inverted clamp (`Math.max(min, …)` guarantees max ≥ min).
- innerWidth ≤ 0: `w = Math.max(1, innerWidth)` → returns 300; no division by zero, no NaN/Infinity propagation.
- All four `appearance.test.ts` expectations verified by hand: (0.4,1000)→0.4; (831/800,800)→0.4; (0.9,2000)→0.5; (0.05,1000)→0.3. ✓

**Percentage base** — App's root is `flex h-screen w-screen` (App.tsx:685), so RightPanel's `width: X%` resolves against the full window width: `frac × innerWidth` = rendered px. The band and the drag math are consistent with what actually renders. (Sidebar `w-14` = 56px and handle `w-1.5` = 6px are flex *siblings*, not part of the percentage base.)

**Drag math** (App.tsx ResizeHandle, :853-901) — `startWidth = clampPanelFraction(frac, innerWidth) × innerWidth` matches the rendered width, so no jump at pointerdown (re-clamping against the *current* viewport is important and correct); the DOM-measurement fallback when frac is null is unchanged from the old code and `nextElementSibling` is indeed the panel root; `fracFromPx` reads the live `window.innerWidth` per call; live path (`onFracChangeLive` → in-memory) vs commit path (`onFracCommit` → persists) correctly match the store setters. Pointer capture, pointercancel routing, and unmount cleanup all preserved.

**Legacy seed** (`readRightPanelWidthFrac`, appearance.ts:260-267) — read-only (no migration write; the legacy key stays and is reinterpreted against the *current* viewport each startup, which is exactly the desired re-normalization); the `typeof window === "undefined"` guard sits before the `window.innerWidth` division (node-env safe); garbage values → null via `Number.isFinite`; frac key takes precedence (unit-tested).

**Store rename completeness** — `rightPanelWidth\b` / `setRightPanelWidth\b` searches over `frontend/**` find only intentional references: the `LS_RIGHT_PANEL_WIDTH` constant, its doc comment, and test fixtures seeding the legacy key. `useAgentStore.preview.test.ts` and `MarkdownLink.test.tsx` never referenced the width field (they use `rightPanelVisible`/`rightPanelTab` only), so the diff correctly leaves them untouched.

**Test quality** — `RightPanel.width.test.tsx` renders the real component through the real store, seeded via fake localStorage + `vi.hoisted` window stub (the established `Conversation.window.test.tsx` node-env pattern; `environment: "node"` confirmed, registered at vitest.config.ts:87). Test 1 (831px @ 800): old code renders `width:640px` → fails both `/%$/` and `≤50`; new code renders `40%` → passes — genuine regression coverage of the changed path. Test 2's percentage-ness assertion at two viewports is the *correct* contract for resize tracking (the string staying "40%" while the effective width scales is the fix; asserting a string change would be wrong). The claimed neutralization proof is consistent with these assertions.

**Docs sync** — module doc comments updated in all changed files (ResizeHandle doc, RightPanel render comment, both LS-key docs, store state/setter docs). `docs/FEATURES.md` has no panel-resize description (its only "panel" hit is the Browser-tab link line); PLAN.md's right-panel lines (175, 327, 893, 1021-1029) describe tabs/behavior, not width. Nothing stale.

**Multi-platform neutrality** — pure web APIs (`window.innerWidth`, `localStorage`, PointerEvent); no platform-specific code; behaves identically in WebView2 and WKWebView.

**File-tools-first** — no signs of shell-based mutation in the diff; the `.coding/backlog.jsonl` addition is app-managed bookkeeping (an unrelated pending backlog item).

**Security** — no injection surface: localStorage values pass through `Number()` + `Number.isFinite` + clamping; style values are numerically derived.

## Findings

### L1 (LOW) — the band clamp is render-time-only: a window shrink leaves a stale, band-violating percentage until the panel next re-renders

- `RightPanel.tsx:32-35` computes `clampedFrac` against `window.innerWidth` at render time, and the fix deliberately adds no resize listener (the render comment: "A percentage width tracks window resizes for free — no listener"). The CSS percentage does re-scale on resize — but the *clamp* does not re-evaluate.
- Reachable scenario: drag the panel to frac = 0.5 on a ≥960px window (allowed: max = min(0.5, (w−480)/w) = 0.5 for w ≥ 960), then shrink the window to the 800px minimum. The panel renders 50% of 800 = 400px; the band at 800 wants ≤ 320px. Chat column = 800 − 56 (sidebar) − 6 (handle) − 400 ≈ 338px — below the "~480px guaranteed minimum" that both the RightPanel comment (:28-29, "can never crowd the chat column below its guaranteed minimum") and `clampPanelFraction`'s doc comment promise.
- Mitigations already present: the stored frac is always ≤ 0.5 (drag commits and the legacy seed are clamped), so the worst case is half the window — not the original 80% pinning; the panel re-clamps on the next change to any of its subscribed store slices (tab toggle, drag, panel hide/show). App.tsx's `onResized` (:604) only persists geometry — it does not re-render the panel.
- Suggested fix (small): a `resize` listener that bumps a store value RightPanel subscribes to (or re-clamps the frac in the store on resize) so the band re-evaluates with the viewport — or soften the "never" in the two comments to "at render/drag time". This is a residual edge of the accepted no-listener design, not a reintroduction of the reported bug — hence LOW.

## Notes (no action required)

- The "~480px chat minimum" is measured from the window width; the actual chat floor is ~418px after the 56px sidebar + 6px handle. Documented with "~" and matches the plan-specified band `(innerWidth−480)/innerWidth`.
- `clampPanelFraction`'s "at most half the window" doesn't hold below a 600px-wide window (the 300px floor wins via `max = Math.max(min, …)`); unreachable in the app (min window 800px, OS-enforced via min_inner_size + the geometry clamp).
- The 0.1%-rounded percentage can render 299.7px where the floor is 300px (e.g. 33.3% of 900); the retained `min-w-[300px]` class catches it (CSS min-width wins over width) — a 0.3px discrepancy.
- `setRightPanelWidthFrac(null)` has no current caller (API parity with the old setter, which the handle never invoked with null either) — harmless.
- The legacy `mh.rightPanelWidth` key is never cleared after seeding — every startup re-seeds from it until the user first drags. That is the documented "no migration write" design and gives the desired re-normalization-against-current-viewport behavior.

## Bug-plan extras

- **Root cause documented in the plan** ✓ — plan Context carries the full root cause with file:line anchors (appearance.ts:30, useAgentStore.ts:664/:814-821/:828, RightPanel.tsx:31-39, App.tsx:823-917/:745), and the Bug section restates both faces.
- **BUG memory 97200cbc exists and is accurate** ✓ — its symptom + root cause match the pre-fix code as verified against the diff's removed lines (px state, px render clamp `[300, 0.8×innerWidth]`, px drag clamp at commit); the knowledge file `.coding/knowledge/bug/2027-01-11-plan-panel-pins-at-its-80-cap-on-startup-persist.md` is accurate including the second-face (resize non-tracking) amendment, and its fix direction (fraction, ~[0.15, 0.5] band, ~480px chat minimum) matches what landed.
- **Regression test** ✓ — `RightPanel.width.test.tsx` exists, is registered in `vitest.config.ts:87`, is named in the plan's Regression test field, and (per the analysis above) fails without the fix / passes with it.
