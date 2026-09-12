## Verdict: PASS

Reviewed ALL uncommitted changes on `wt/agenticcoder`: `git diff HEAD` (5 modified files) + 4 untracked files (`SplashCard.tsx`, `SplashCard.test.tsx`, the successor SPEC record, the plan file). Plan ef6828ba — splash-branded startup dialogs. The change is correct, behavior-preserving where it must be, well tested, and closes a real prior review finding. No findings.

## Scope verified

### 1. Rules of hooks — clean
- `App.tsx:121`: `useBrowserOverlay(reconcile !== null)` sits top-level among the other hooks, unconditionally, well before the early returns (`needsProject` at :569, `startupError` at :574). The explanatory comment is accurate.
- `SplashCard.tsx:41`: single `useId()` at the top; no conditional hooks. `MnemoLogo` nested inside `SplashCard` calls its own `useId()` — separate hook instances, both sound; sibling `useId` values are unique per component instance, so a card + its logo can never collide.
- Edge case considered, not a finding: if `reconcile` were non-null while an early-return branch rendered, the overlay-enter would be held without the dialog visible. Those states are mutually exclusive in practice (reconcile events require an open project; `needsProject` = no project; `startupError` = brain failed before reconcile runs), and the backend depth counter is saturating/balanced, so even the impossible case is harmless.

### 2. SVG/markup correctness — clean
- `viewBox="0 0 384 144"` + `preserveAspectRatio="xMidYMid slice"` on a `w-96` (384px) `h-36` (144px) band is a 1:1 mapping at the default width; `slice` crops gracefully for any other `className` width.
- Def-id namespacing: SplashCard's `${uid}-splashglow` (with `:` stripped) and MnemoLogo's `${uid}-bg/-glow/-blur` — distinct suffixes, per-instance prefixes. The test at `SplashCard.test.tsx:40-58` renders two cards in one tree and asserts 2 distinct `url(#…)` refs that both resolve to existing `id="…"` defs — exactly the simultaneous-mount scenario (IndexingOverlay + reconcile dialog share App's tree).
- `aria-hidden="true"` on the decorative network SVG (`SplashCard.tsx:60`); `MnemoLogo` keeps `role="img"`/`aria-label`. Card root has `overflow-hidden rounded-xl`, so the band's un-rounded top corners are clipped — the plan's `rounded-t-xl` is correctly unnecessary.

### 3. Behavior parity — clean
- Indeterminate path: `total === 0` → `right="…"`, `pct={null}` → `width:30%` — byte-identical semantics to the deleted inline blocks. `pct=40` → `width:40%` (pinned by tests).
- Spinner: `animate-spin` gated on `reconcile.phase === "running"` / `state.kind === "progress"` — unchanged.
- done/failed branches: green summary / red error blocks and both Dismiss buttons (`setReconcile(null)` / `setState(null)`) untouched, still inside the content slot; the splash band now also shows in done/failed — matches plan step 4(b) intent.
- Children slot renders below the band (test asserts marker appears after "mnemo" in document order). Old root `p-5` moved to the content slot so the band is full-bleed — intended.

### 4. No leftovers — clean
- Full-text search for the deleted strings ("on-disk knowledge changed", "First-time indexing") hits only `.coding/` records (plan + SPEC, which quote them intentionally as history). Zero source references.
- `IndexingOverlay.tsx:45-63` doc comment describes "a full-screen modal with a progress bar and a mono 'N/M files indexed' counter" — still accurate; it never described the dropped paragraph. The trailing-newline-less EOF is unchanged from HEAD (context-line marker in the diff), so line-ending style is preserved.

### 5. Constitution checks — clean
- **Documentation sync:** both README bullets (~L43 reconcile, ~L52 indexing overlay) accurately describe the branded splash + bar + counter as implemented. PLAN.md contains no rows for these dialogs and the prior overlay review established "no row required" for this UI layer — none needed here. SPEC supersede chain is correct: old record carries `status = "superseded"`, the `-2` successor carries `supersedes = "…bar-co"` and documents the splash rebuild + the NOT-yet-merged branch marker.
- **Multi-platform neutrality:** pure SVG/Tailwind/React — no OS APIs, paths, or shell. The `useBrowserOverlay` addition uses the existing sanctioned WebView2 abstraction; no new `cfg(windows)` surface.
- **Doc comments:** both exported components have full doc comments; every props field is documented. No Rust changes → no `#[allow]`/warnings surface.
- **No silent test loss:** `frontend/vitest.config.ts:24` registers `SplashCard.test.tsx` in the `include` array (alphabetical position, matching convention).
- **Prior-finding closure verified:** `.coding/reviews/2026-indexing-progress-overlay.md:21` (LOW 2) explicitly noted "The reconcile dialog at `App.tsx:596` has the same pre-existing gap" — the new `useBrowserOverlay(reconcile !== null)` closes exactly that, and the plan/spec accurately cite it.

### 6. Tests genuinely cover the change
The 8 new tests would fail if: the brand stack were removed (wordmark/tagline/values assertions), the palette regressed (4 hex-color assertions), ids stopped being namespaced (two-instance uniqueness test), the children slot moved above the band (document-order assertion), or the indeterminate stub broke (`pct=null` → `width:30%`). Reported run: 48 files / 628 tests green, `npm run build` (tsc + vite) green, `cargo test` green under `#![deny(warnings)]`.

## Notes (informational, not findings)
- `SplashCard`'s default `className = "w-96"` preserves both dialogs' previous width; neither caller overrides it, so visual width is unchanged.
- `.coding/plans/ef6828ba.md` all 5 steps checked off; the implementation matches the plan text (including the doc-comment nuance in step 3, which was satisfied because the comment never described the removed paragraph).
