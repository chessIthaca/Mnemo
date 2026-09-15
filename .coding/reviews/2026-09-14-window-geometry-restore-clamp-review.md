## Verdict: FINDINGS (0 high, 8 low)

The fix is **sound and correct in its core**: the clamp math checks out on every case I recomputed by hand, the user-reported regression is pinned by a real test, the resize path and the Rust `min_inner_size` are genuinely untouched, and the change is multi-platform neutral. All 8 findings are low — prose accuracy, one corner of the "fully on screen" guarantee, a units nit, one dead knob, two degradation-path robustness items, test coverage gaps, and plan bookkeeping. Nothing here blocks the fix; the chrome conversion in particular is correct and should stay (finding 1 only asks to fix the *direction* stated in its rationale).

Scope reviewed: `git diff HEAD` (5 modified files) + the 2 untracked new frontend files (`.coding/plans/9ff59133.md` read as plan context). Root cause claim verified against the vendored tao tree as requested.

---

## What I verified (no action needed)

**Root-cause chain — CONFIRMED, exactly as claimed.**
1. `set_min_inner_size` (vendor/tao/src/platform_impl/windows/window.rs:317-329) only stores `window_state.size_constraints` (+ a re-set of the current size; it consults nothing).
2. The constraints are consumed solely in the `WM_GETMINMAXINFO` handler (vendor/tao/src/platform_impl/windows/event_loop.rs:1888-1938) → `ptMinTrackSize`/`ptMaxTrackSize`, i.e. a *tracking* size.
3. `set_inner_size` (window.rs:273-314) → `util::set_inner_size_physical` (util.rs:91-118) → `AdjustWindowRectEx` + an explicit `SetWindowPos(...)` that never reads `size_constraints`.
   Caveat I want on record: whether USER32 itself applies the track size during `SetWindowPos` is outside the vendored tree, but the user's observed sub-minimum startup window is direct evidence the restore is not clamped — and the fix is correct either way (it clamps regardless of what the OS does).

**The chrome conversion is the right direction.** The save stores OUTER bounds (`App.tsx:557 outerSize()`); the restore calls `win.setSize(...)`. tao exposes **no outer-size setter at all** (vendor/**: `set_inner_size` / `set_min_inner_size` / `set_max_inner_size` only — no `set_outer_size`), so Tauri's `setSize` can only route to `set_inner_size`, which expands the requested value to the frame. Hence `inner = savedOuter − chrome` is exactly what makes a restore→save cycle stable; and for a standard decorated window the two derivations of the chrome agree (`GetWindowRect − client` ≈ `AdjustWindowRectEx` = 16×39 at 100% DPI), so no residual drift to report. The clamp's min is applied to the **inner** size — the same unit as `min_inner_size(800.0, 560.0)` (src-tauri/src/main.rs:244) — so there is no double-subtraction of the chrome. ✓

**Math spot-checked case by case** (I re-derived all 11 unit expectations from `clampRestoredGeometry` by hand — every one is correct, including the boundary `overlap == 80` case at test :118-124, the largest-overlap anchor at :135-144, and the workArea-vs-full-monitor pair at :146-164). Degenerate inputs that ARE handled: negative/zero saved sizes (floored by `Math.max(innerW, 1)` + the min clamp), a chrome larger than the saved size (guarded at windowRestore.ts:182-184), a work area smaller than the min (`maxW = Math.max(minW, …)`), monitor work areas smaller than the window (`maxX/maxY = Math.max(area.x, …)` keeps the title bar at the work-area corner — a good, deliberately-pinned property), and monitors at negative coordinates.

**Scope/behaviour guards intact:** the resize path is untouched (the diff only rewires the mount restore; the `save()`/`debouncedSave`/listener/`beforeunload` block at App.tsx:549-618 is byte-identical), the `maximized` skip is intact (:553-555), the cached-bounds `beforeunload` write is intact (:606-609), the `restoreDone` gate is intact, and no Rust file was touched (no `min_inner_size`/resize change anywhere in the diff) — exactly what the user asked for. `PersistedGeometry`'s new doc comment (:42-46) is accurate.

**Capabilities:** `core:window:default` + `core:window:allow-set-position` + `allow-set-size` (src-tauri/capabilities/default.json:10-12) cover the rewired path — `outerSize()`/`outerPosition()`/`availableMonitors()` already worked pre-change, and the inner-size getter belongs to the same default getter set, so no capability addition is needed.

**Multi-platform (constitution):** clean. The helper imports nothing platform-specific (no `@tauri-apps/api`), every window API used is cross-platform Tauri v2, `workArea` is runtime feature-detected with a full-monitor fallback (windowRestore.ts:133-138), and there is no `cfg(windows)`-only or Windows-path assumption anywhere in the change. The structural `MonitorGeometry` mirror does accept Tauri's `Monitor[]` (tsc green is the empirical proof; the shapes line up field-for-field).

**Docs sync:** `README.md` and `PLAN.md` carry no window-geometry statement at all, so nothing there is stale; `docs/FEATURES.md:62` is in the right section (Agents & interface, next to the UI bullet) and is broadly accurate — the only inaccuracies are findings 1 and 3. Module doc comments: every exported symbol has one, so the constitution's doc-comment rule is met; the helper-as-own-module split is justified (pure, node-testable math with the App.tsx effect kept thin, matching the repo's `delegationNotes`/`markdownLink` pattern).

**Test wiring:** the new file is registered in the explicit allow-list (frontend/vitest.config.ts:90) and the include-list guard (src/lib/vitestInclude.test.ts) already covers it, so the suite cannot silently skip it. The static source-contract pins follow the repo's established `?raw` style (delegationNotes.test.ts) — keep them; two nits in finding 7.

---

## Findings

### L1 (low) — The chrome-drift direction is stated backwards in three places
**Files:** `frontend/src/lib/windowRestore.ts:26-29`, `frontend/src/App.tsx:522-524`, `docs/FEATURES.md:62`

All three say the window "would shrink by the title-bar/border height on every restart" without the conversion. With the save storing the **outer** size (`App.tsx:557`) and the restore calling **`setSize`** (tao's inner size — see the verified section: tao has no outer-size setter, and `set_inner_size_physical` *expands* the requested value to the frame), `inner := savedOuter` made the window **grow** by the chrome (~16×39 logical px at 100% DPI) per restart. The conversion itself is exactly right (outer − chrome = inner → stable round trip) — only the "shrink" wording is inverted, and it is the stated rationale for the conversion, so a future reader re-deriving the decision would reach the wrong conclusion.

**Suggested fix:** say "grows" (and in FEATURES.md "so the window no longer drifts by its title-bar/border height on every restart"). If the parent wants live certainty, two consecutive launches comparing `(await win.outerSize())` settles the direction in seconds — worth 30 s given this sentence is the only justification for the conversion.

### L2 (low) — The dropped-position path can still land the clamped window partly off screen
**Files:** `frontend/src/lib/windowRestore.ts:197`, `frontend/src/App.tsx:535-540`

When no monitor clears the gate the helper returns `x/y = null` and the caller keeps the window at its **OS default origin** — the `.center()` position computed for the initial 1200×720 window (`src-tauri/src/main.rs:243-245`), which is NOT re-centered for the clamped size. Reachable case (monitor unplugged — the classic RDP case the docs name): saved outer 2000×1200 at x=2000, restored on a now-single 1920×1080 screen → inner 1904×1041 placed at the 1200×720 centered origin (360, 180) → the right/bottom edges land ~360 px / ~180 px off screen. The title bar stays inside the work area so the window is draggable and recoverable, but this is the one path where the stated "shows fully on screen" guarantee does not hold. The previous code had the same gap (it also left the origin alone), so this is not a regression — it is the remaining hole in the goal sentence.

**Suggested fix:** when `monitors.length > 0` but no monitor clears the gate, use the sizing monitor's work-area origin (top-left) as the position instead of `null` — that guarantees full visibility without needing an extra API call. Keep `null` only for the empty-monitor case, where the centered default really is the best available.

### L3 (low) — MIN_VISIBLE_PX's doc claims parity with the pre-clamp gate; the units actually changed
**Files:** `frontend/src/lib/windowRestore.ts:50-55` (+ `docs/FEATURES.md:62` "80 px of overlap")

The comment calls the constant "the check the pre-clamp restore already used", but the old gate compared `overlap >= 80 * factor` **physical** px — i.e. 80 **logical** px — while the helper compares a constant 80 **physical** px (= 80/factor logical). At factor 1 they coincide; at factor 2 the new gate is half as strict in logical terms (40 logical px of overlap). The user-visible consequence is mild (the position is pulled fully inside the work area either way; only "keep the saved position" vs "center instead" flips), so this is a parity/doc nit, not a defect.

**Suggested fix:** either restore exact parity (`const minOverlap = MIN_VISIBLE_PX * factor`) or correct the comment — and make FEATURES.md say physical px, or just "80 px" → "a usability gate". (Note the test at `windowRestore.test.ts:126-133` pins the constant, so either choice stays covered at factor 1.)

### L4 (low) — `minOverlapPx` is dead, untested surface
**File:** `frontend/src/lib/windowRestore.ts:100-101`

No caller and no test uses the override (I grepped the test file: zero occurrences). A knob nothing pins is where a future refactor silently changes behaviour.

**Suggested fix:** drop it, or add one case exercising it (e.g. `minOverlapPx: 200` turning the boundary case at test :118-124 into a centered fallback).

### L5 (low) — No guard on a non-positive / non-finite `factor`
**Files:** `frontend/src/lib/windowRestore.ts:187-206`, `frontend/src/App.tsx:514-517`

The helper divides by `factor` in five places with no positivity guard: `factor === 0` yields `Math.round(widthPhys / 0)` = `Infinity` (or `NaN` where the numerator is also 0), which then reaches `new LogicalSize(...)`/`setPosition` and rejects at the IPC — the outer `catch` logs "failed to restore window geometry" and the window keeps the 1200×720 default. So it degrades, but only after a failed round-trip and with a confusing log. App.tsx degrades a *rejected* `scaleFactor()` to 1 but not a resolved `0`/`NaN`.

**Suggested fix:** one line at the top of the helper, e.g. `const f = Number.isFinite(factor) && factor > 0 ? factor : 1;`. (The pre-existing save path divides by the factor too — `App.tsx:562-565` — so this is not a new class of risk; it just became worth pinning now that the factor is load-bearing for the clamp.)

### L6 (low) — An empty monitor list floors the size to the minimum instead of merely bounding it
**Files:** `frontend/src/lib/windowRestore.ts:187-190`, `frontend/src/App.tsx:518-521`

With `monitors: []` the degrade path passes an empty list, so `maxW/maxH` become exactly `minW/minH` **and** `innerW/innerH` are clamped up into that degenerate range: a perfectly good 1600×1000 save is forced down to 800×560. The App.tsx comment ("a failed monitor enumeration … still leaves the size clamped") describes this as clamping, but it is a floor. Shrinking a valid window because an *unrelated* API failed is the wrong trade — the position is dropped on that path anyway.

**Suggested fix:** when `monitors.length === 0`, bound the size by the minimum only (`max = Number.POSITIVE_INFINITY`, or simply skip the max clamp) so an unreadable monitor list cannot shrink the window; keep the work-area bound whenever a sizing anchor exists.

### L7 (low) — Test coverage gaps (and two pin nits)
**File:** `frontend/src/lib/windowRestore.test.ts`

The 11 unit cases pin the user-reported defect properly (the 300×200@(0,0) → 800×560 case at :70-77) and cover the helper's main rules; the static pins are in the repo's established `?raw` contract style and each maps to a real requirement — keep them. Missing cases, in rough priority order:
1. **factor ≠ 1 combined with chrome** — :92-99 uses factor 1 and :101-109 uses chrome {0,0}; the two paths are never exercised together, though that is exactly the shape of a real cross-DPI restart.
2. **Monitors at negative coordinates** (a display left of/above the primary) — the code handles it (`maxX = Math.max(area.x, …)` puts the title bar at the work-area corner), but nothing pins it.
3. **Min-floor wins with a smaller work area *and* non-zero chrome** (:84-90 covers only the chrome-free variant) — the position result there depends on `widthPhys + chromeW` overshooting the area, which is the subtlest branch in the function.
4. **Mixed-DPI layout** — the helper deliberately uses the *window's* factor for the saved rect and ignores each monitor's own `scaleFactor` (the same approximation the pre-change code made). That is a defensible choice but it is undocumented and untested; one case with two monitors at different scale factors would pin the intent.
5. **Hand-edited negative/zero saved values** — handled (`Math.max(innerW, 1)` then the min clamp) but unpinned. (`NaN`/`Infinity` cannot arrive through JSON/localStorage — `readWindowGeometry`'s `typeof "number"` check is sufficient there — so no case needed for those.)
6. **Pin nits:** `not.toContain("setMinSize")` (:192) fails on ANY future legitimate `setMinSize` call anywhere in App.tsx, and `not.toContain("clampRestoredGeometry(await")` (:193) does not express what its title claims ("no runtime min size / resize clamp") — it is near-vacuous. Consider replacing the latter with a "called exactly once, inside the mount effect" assertion, and scoping/softening the former.
7. *(Optional)* The mirrored constants (`windowRestore.ts:44-48` = 800/560 vs `main.rs:244` min_inner_size(800.0, 560.0)) are asserted only in prose, in three places; nothing fails if either side changes. A static pin in the existing contract style would close that — judge whether it is worth the cross-root `?raw`/fs-allow plumbing.

### L8 (low) — Plan bookkeeping: feature-scale trigger and a stale detailed step
**Files:** `.coding/plans/9ff59133.md` (context + Detailed steps)

The fix introduces a new module with exported types/constants consumed by another module (`windowRestore.ts:90-208` → `App.tsx:24, 534`) — the plan's own step 4 names "new pub types or cross-module surface" as the feature-scale trigger, which requires a **"Landed design —"** context amendment + `landed_design=true` before the verify step (then a SPEC memory + BUG amendment at finish). The plan context currently carries no such amendment. Either add it (there is real design substance to record: the inner-vs-outer unit convention, the sizing-vs-position anchor split, the min-floor-wins rule, the degradation matrix) or record explicitly why this stays a contained defect fix. Separately, Detailed-steps item 4 ("Docs + full verify") is still unchecked while the FEATURES.md bullet and all three verification runs are demonstrably done — tick it so the crash-resumption document matches reality.

---

## Non-blocking notes

- **Commit contents.** Besides the five clamp files, the uncommitted tree carries unrelated `.coding` bookkeeping — `backlog.jsonl` (new steering-note item), `plans/ee65fd4b.md` (step 8 ticked) — plus three untracked knowledge records (this fix's min_inner_size SPEC and two from earlier plans). By design those travel with the mergeable side-car, so this is expected; just be aware the fix's commit will carry them.
- **Silent degradation if the chrome probe ever fails.** If `innerSize()`/`outerSize()` were unavailable, `App.tsx:530-533` logs and continues with chrome {0,0}, so the restore would come back chrome-larger instead of exact — a console-only signal. Acceptable for a degradation path, but worth knowing that the FEATURES.md "no longer drifts" claim depends on that probe succeeding. (Grants do cover it today — see the verified section.)
- **Nothing in the change makes the startup-only scope ambiguous**: the module doc (:21, :35-37), the App.tsx effect comment (:508-509) and the FEATURES.md sentence all state it, the helper is called exactly once inside the mount restore, and the diff touches no resize listener, no `setMinSize`, and no Rust file. The one thing a future reader could misread is the drift direction — L1.

**Constitution compliance:** no `#[allow]`, no shell-based file mutation, no commit to main, doc comments on all exported symbols, regression test present and pinned to the user's report, multi-platform neutral, no Windows-only additions. `cargo test`/vitest/build results are as reported by the parent (I could not re-run them read-only; I re-derived the unit expectations by hand instead).
