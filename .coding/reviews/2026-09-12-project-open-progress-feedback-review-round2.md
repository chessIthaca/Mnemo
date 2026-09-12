## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of plan 969510fc "Project open/create progress feedback (486955d5)" at commit 18de87a (HEAD on wt/agenticcoding; working tree completely clean — `git diff HEAD` and `git status` both empty, so the on-disk state IS the commit). Both round-1 fixes are present and correct: the five static-splash values are aligned to the Tailwind utilities with the parity contract pinned in the HTML comment, and the two source-contract describe blocks (2 + 3 tests) live in the already-registered test files. Every numeric value in the static splash now matches its BootSplash Tailwind counterpart (verified against Tailwind v3.4.15 defaults — the config extends only colors), and nothing else in the commit drifted from the round-1-reviewed tree (line-number-level cross-check: App.tsx :595/:603/:608, vitest.config.ts :26/:86, ProjectPicker wiring, BootSplash.tsx, projectPickerPhases.ts, README clause — all identical to round-1's evidence). One LOW finding: the new handleOpen source-contract guard is not bounded to handleOpen's own `switchProject` call, so the exact regressions it exists to catch (dropping/reordering handleOpen's `setPhase("restarting")`, deleting its `await switchProject`) still pass the entire suite undetected.

### Findings

**LOW 1 — The handleOpen source-contract guard is unbounded: handleCreate's later occurrences satisfy it.**
File: `frontend/src/components/projects/projectPickerPhases.test.ts` (:61-68).

The test searches forward from `async function handleOpen(` with no upper bound:

```ts
const openFn = pickerSource.indexOf("async function handleOpen(");            // :78
const restarting = pickerSource.indexOf('setPhase("restarting")', openFn);    // :79 (handleOpen's)
const switchCall = pickerSource.indexOf("await switchProject(", restarting); // :82 (handleOpen's)
```

Both needles also occur later in handleCreate (`setPhase("restarting")` :130, `await switchProject(` :131), so mutations confined to handleOpen fall through and the test still passes:

- **Delete handleOpen's `setPhase("restarting")` (:79)** — `restarting` falls through to :130, `switchCall` to :131; `131 > 130 > 78` → passes. The regression (Open never shows "Restarting…", buttons never disable during open) is caught by nothing else: no other test imports `ProjectPicker.tsx?raw` or renders the picker, the walk-order test starts at `setPhase("preparing")` (:124), and the reset count is unchanged (handleOpen's catch `setPhase(null)` :87 remains). The full suite stays green.
- **Move `setPhase("restarting")` below its `await switchProject(path)`** — `restarting` finds its own new position, `switchCall` falls through to :131 → passes.
- **Delete handleOpen's `await switchProject(path)` entirely** — `switchCall` falls through to :131 → passes.

This is the mirror image of the hole the walk-order test explicitly guards against (its comment: "Each search starts after the previous anchor so handleOpen's earlier restarting/switchProject can't satisfy it") — backward fall-through is anchored away, forward fall-through is not. Round-1's LOW-2 was precisely that the wirings were "pinned by nothing"; for handleOpen's restarting phase they still effectively are.

Fix (one token): search for handleOpen's own call — `await switchProject(path)` (the `path` argument is unique to handleOpen; handleCreate passes `chosenPath`):

```ts
const switchCall = pickerSource.indexOf("await switchProject(path)", restarting);
```

All three mutations then fail (`switchCall` = -1 or the ordering breaks). Alternatively, bound both searches to the handleOpen body (between `async function handleOpen(` and `async function handleRemove(`).

### Requested checks — evidence

**1. Both fixes present and correct in the committed source.**

*LOW-1 (splash parity) — fixed exactly as prescribed (the reviewer's first option: static values aligned to the Tailwind utilities).*
- `frontend/index.html` (current on-disk state = commit 18de87a): wordmark `font-size:1.5rem` / `letter-spacing:0.1em` (:43), spinner `width: 20px; height: 20px` + `animation: mnemo-boot-spin 1s linear infinite` (:16-17, :21), "Starting…" `font-size:0.875rem` (:48). All five round-1 deltas corrected: 1.6rem→1.5rem, 0.12em→0.1em, 22px→20px, 0.9s→1s, 0.8rem→0.875rem.
- The HTML comment (:35-38) pins the parity contract as described: "Numeric values are aligned to the React twin's Tailwind utilities (text-2xl / tracking-widest / h-5 w-5 / animate-spin / text-sm) so the handoff is pixel-identical (review LOW 1, plan 969510fc)".
- `BootSplash.tsx` unchanged (the reference) — its class list (:22-29) is identical to round-1's description.

*LOW-2 (regression guards) — both describe blocks present, in the already-registered files, following the repo's `?raw` source-contract pattern (cf. App.shellRender.test.ts).*
- `BootSplash.test.tsx` :25-40 — "App boot-splash wiring (source contract, review LOW 2)": (a) `indexOf("if (!checkedStartup) {")` < `indexOf("if (checkedStartup && needsProject) {")` — both needles occur exactly once in App.tsx (:595 and :603, verified by literal search), so the ordering assertion is deterministic and fails if the early return drifts below the picker gate; (b) `toContain("return <BootSplash />;")` — present at App.tsx:596.
- `projectPickerPhases.test.ts` :35-68 — "ProjectPicker phase wiring (source contract, review LOW 2)": (i) the handleCreate walk chain — `setPhase("preparing")` :124 → `await createProject(` :128 → `setPhase("restarting")` :130 → `await switchProject(` :131, each search anchored after the previous (traced: passes; robust — dropping or reordering any step breaks the chain or yields -1, and handleOpen's earlier occurrences at :79/:82 cannot satisfy it); (ii) exactly 3 `setPhase(null)` — handleOpen catch :87, handleRemove finally :100, handleCreate catch :134, the only three in the file (regex-counted); (iii) the handleOpen guard — present and passing on the current source (:79 < :82), but not bounded to handleOpen's own switchProject → LOW 1 above.
- No new test files → no vitest.config.ts change needed: the current config carries exactly the two round-1 registrations (`src/components/common/BootSplash.test.tsx` :26, `src/components/projects/projectPickerPhases.test.ts` :86), nothing added, nothing duplicated.

**2. The fixes introduced no regressions.**
- The index.html changes are pure CSS value alignments plus an inert comment — no logic surface. The test additions are test-only. No production file changed post-round-1 except index.html.
- Test arithmetic is consistent with the parent's report: +5 new `it` blocks (2 in BootSplash.test.tsx's new describe + 3 in projectPickerPhases.test.ts's), 1102 + 5 = 1107, file count unchanged at 81 (no new files).
- Every new assertion was traced against the on-disk source and passes deterministically (exact-index reasoning above); the guards fail on the round-1-described regressions they cover (early return moved below the picker gate → ordering fails; any `setPhase(null)` dropped or added → count fails; handleCreate walk dropped/reordered → chain breaks).
- tsc: the `?raw` imports are typed by `vite/client` (the established pattern — App.shellRender.test.ts:27 documents it; 30+ test files use it), so `npx tsc --noEmit` has no new surface from the fix.
- Read-only reviewer: the suite was not re-run here; the parent's reported green run (81 files / 1107 tests) is consistent with the +5 arithmetic and the traced assertions.

**3. Nothing else in 18de87a regressed relative to the round-1-reviewed tree.**
- Working tree completely clean (`git diff HEAD` empty, `git status --short` empty) — the on-disk state IS commit 18de87a, which is HEAD on wt/agenticcoding.
- The commit's 12 files reconcile exactly with round-1's reviewed set (6 modified + 5 untracked) plus `.coding/plans/969510fc.md` (the app-managed plan file): modified — README.md, frontend/index.html, frontend/src/App.tsx, ProjectPicker.tsx, vitest.config.ts, .coding/backlog.jsonl; untracked — BootSplash.tsx, BootSplash.test.tsx, projectPickerPhases.ts, projectPickerPhases.test.ts, the round-1 review report. No extra source files.
- Line-number-level cross-check against round-1's evidence — all identical, i.e. zero post-round-1 drift beyond the two fixes: App.tsx early return :595 (after the geometry useEffect ending :589), picker gate :603, error screen :608; vitest registrations :26/:86; ProjectPicker's full wiring (handleOpen restarting + catch-reset, handleRemove removing + finally-reset, handleCreate preparing→restarting + catch-reset, `busy = phase !== null`, label wiring on both buttons, all four `disabled={busy}`); BootSplash.tsx class list; projectPickerPhases.ts mapping table; the README clause (matches round-1's quoted content verbatim).
- The only post-round-1 source deltas are therefore exactly the index.html value alignments + parity comment and the two test-file describe blocks — as claimed.

**4. The parity claim is now literally true.**
Verified value-by-value against Tailwind v3.4.15 defaults (package.json) with the config checked for overrides — `tailwind.config.ts` extends only colors (bg/border via CSS vars), so every default scale value stands:

| static splash (index.html) | BootSplash utility | resolved value | match |
|---|---|---|---|
| gap:0.75rem | gap-3 | 0.75rem | ✓ |
| font-size:1.5rem | text-2xl | 1.5rem | ✓ |
| font-weight:600 | font-semibold | 600 | ✓ |
| letter-spacing:0.1em | tracking-widest | 0.1em | ✓ |
| spinner width/height:20px | h-5 w-5 | 1.25rem = 20px | ✓ |
| border:2px | border-2 | 2px | ✓ |
| border-radius:50% | rounded-full | 9999px | ✓ (identical circle on a 20×20 box) |
| animation 1s linear infinite | animate-spin | spin 1s linear infinite | ✓ |
| font-size:0.875rem ("Starting…") | text-sm | 0.875rem | ✓ |
| background:#0f172a | bg-bg-primary | var(--bg-primary) = #0f172a (dark, globals.css:11) | ✓ |
| color:#e2e8f0 | text-slate-200 | #e2e8f0 | ✓ |
| border #334155 / top #22d3ee | border-slate-700 / border-t-cyan-400 | #334155 / #22d3ee | ✓ |
| color:#94a3b8 | text-slate-400 | #94a3b8 | ✓ |
| font-family:system-ui,sans-serif | (inherits --app-font-family) | system-ui, -apple-system, "Segoe UI", Roboto, sans-serif (globals.css:7) | ✓ (system-ui resolves first on both platforms — same rendered face) |
| position:fixed;inset:0 | h-screen w-screen | 100vh/100vw | ✓ (both full-viewport) |

Every numeric value in the static splash matches its BootSplash Tailwind counterpart — the five round-1 deltas are all closed. Two residual observations, both below finding threshold: (a) Tailwind's text-2xl/text-sm carry line-heights (2rem/1.25rem) that the static splash does not set (browser `normal`) — a few-px line-box difference at handoff; round-1 reviewed the splash layout in full and scoped LOW-1 to the five differing values, and the parity claim as defined (every numeric value present in the static splash) is literally satisfied; (b) 50% vs 9999px border-radius renders identically on the 20×20 spinner.

### Other review dimensions

- **Multi-platform neutrality:** the round-2 deltas are CSS values in a static splash and test-only code — no platform surface; the font stacks resolve identically on WebView2 (Windows) and WKWebView (macOS).
- **Docs sync:** the HTML comment now documents the parity contract in the right place (next to the values it pins); the README's "matching BootSplash" wording — round-1's only stretch — is now accurate. No other docs touch these values.
- **Constitution:** no shell mutation in the diff; the new tests follow the repo's source-contract conventions (copyright header, intent-bearing describe/it, anchored indexOf chains); no `#[allow]`/warning surface.
- **Verification reliance:** tests were not re-run by this read-only reviewer; the parent's 81-files/1107-tests green run is consistent with the +5 arithmetic, and every new assertion was traced to pass against the committed source.
