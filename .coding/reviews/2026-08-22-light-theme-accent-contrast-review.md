# Review: Readable light theme + accent-contrast fix (plan 31af61d0)

Scope: uncommitted diff on `fix/compact-bugs` vs HEAD (globals.css, appearance.ts, themeContrast.test.ts [new], appearance.test.ts [new], vitest.config.ts, README.md, plus `.coding/*` harness bookkeeping). Committed history not reviewed.

## Verdict

Core implementation is **correct and well-tested** — cascade reasoning, JS accent resolution, all preview/reset/mount flows, and the WCAG math check out. Findings are **two minor doc/consistency issues** (inaccurate contrast-ratio comments; a stale in-app "Light preset" that reintroduces sub-AA colors), plus informational notes. No correctness bugs, no security issues, no platform-specific code.

---

## Findings

### MINOR 1 — Contrast-ratio comments are inaccurate (constitution: doc/comment accuracy)

Verified by recomputing WCAG 2.x luminance/ratios from the shipped tokens:

1. **globals.css:23-24** — comment says dark slate `#0f172a` on cyan-400 `#22d3ee` is "≈10.6:1"; actual is **≈9.9:1** (L1=0.00881, L2=0.53095 → 0.58095/0.05881=9.88).
2. **globals.css:41** — comment says cyan-700 `#0e7490` on white is "≈5.9:1"; actual is **≈5.4:1** (L=0.14587 → 1.05/0.19587=5.36). Worse, this **contradicts globals.css:53**, which correctly says "≈5.4:1" for the same color pair (contrast is symmetric) — two different values for one pair in adjacent blocks.
3. **frontend/src/hooks/appearance.test.ts:25-26** — repeats the "≈5.9:1 on white" error (should be ≈5.4:1).
4. Trivial: "≈1.9:1" for cyan-400 on white appears in appearance.ts:40, appearance.ts:62, globals.css:42, themeContrast.test.ts:9, appearance.test.ts:8 — the computed/published value is **1.81:1**. Given the "≈" this is borderline, but 1.8 is the accurate figure and the rest of the comments quote one-decimal precision.

None of these affect test assertions (all floors pass with margin — see "Verified" below); they are comment-accuracy fixes only.

### MINOR 2 — "Light preset" in AppearanceSection reintroduces the sub-AA palette the plan fixed

`frontend/src/components/settings/sections/AppearanceSection.tsx:296-316` — the **"Light preset"** button (untouched by this diff) patches `accentColor: "#0891b2"` (cyan-600, **≈3.7:1 on white** — below the 4.5:1 floor this plan establishes) and `textMutedColor: "#64748b"` (slate-500, **≈4.35:1 on bg-secondary #f1f5f9** — the exact sub-AA muted value the plan deepened to `#475569`). One click in the light theme puts back the "terrible and unreadable" scheme from the user report. Recommend updating the preset to the new tuned tokens (`#0e7490` / `#475569`, or simply aligning it with the `html.light` block). Note the preset is a customized accent (≠ `#22d3ee`), so it applies as-is in both themes and never benefits from the swap.

### Informational (no action required)

- **`.bg-cyan-600` blast radius — verified safe.** All solid `bg-cyan-600` usages pair with `text-white` (App.tsx:531, ErrorBoundary.tsx:45, Message.tsx:173, MergeToMainDialog.tsx:100, BacklogView.tsx:214/551/719, InputBar.tsx:766-771), so the `color: var(--accent-contrast-text) !important` flip is uniformly an improvement (dark mode: 1.8→9.9:1). The user-message bubble (Message.tsx:173) also flips to dark-slate text in dark mode — intended per the plan ("Send etc. … BOTH themes"). Translucent variants (`bg-cyan-600/10`, `/20`, `/40`, `border-cyan-600/40` in InflightBar.tsx:413, InputBar.tsx:686/741, BacklogView.tsx:204) are **distinct Tailwind classes** (`bg-cyan-600\/20` ≠ `bg-cyan-600`) and are unaffected by the new `color` declaration.
- **Test parser fragility is theoretical only.** `blockBody` uses `indexOf(":root")` / `indexOf("html.light")`; today no comment precedes either block containing those strings (the line-164 comment mentioning `html.light` is *after* the line-44 block). If a future comment collides, extraction lands in the wrong place and `tok()` throws on the missing token — fail-loud, never a silent pass. Acceptable as-is.
- **themeContrast.test.ts:10-11** docstring says it "parses the CSS source (`?raw`)" although it actually reads from disk via `node:fs` (because `?raw` is stubbed empty) — the very next lines (19-22) explain this accurately, so it self-corrects. Trivial wording nit at most.
- **Accent ColorRow swatch** shows the *stored* value (`#22d3ee`) even in the light theme where the effective accent is `#0e7490`. This is the correct choice (rendering the effective value in an `<input type="color">` would write the swapped value back as a "customization" on the next patch), but the cosmetic mismatch may briefly surprise. No change needed.
- **color-mix() in keyframes** (globals.css:221/225) adds no new browser requirement: `color-mix(in srgb, …)` is already load-bearing at globals.css:73-75 (`.bg-cyan-600\/20` etc.). Same support matrix (Chromium ≥111 / Safari ≥16.2).
- **Custom-accent contrast is the user's responsibility:** e.g. picking the light default `#0e7490` as a custom accent and switching to dark yields ≈3.3:1 accent text. Pre-existing class of behavior for any custom color; Settings shows a live preview. The regression test rightly guards only the shipped defaults.
- `.coding/backlog.json`, `.coding/plans/stack.json`, and the new untracked plan file are harness bookkeeping — fine to include in the commit.

---

## Verified correct (explicitly checked per the review brief)

- **Cascade/override:** inline `--accent-color` from `applyTheme`/`applyColors` always outranks the `html.light` stylesheet declaration, making JS the authoritative accent path (the stylesheet light accent is only a pre-hydration fallback). `--accent-contrast-text` is never written inline, so it cascades correctly: `html.light` (0-1-1) beats `:root` (0-1-0). Customized accents apply as-is in both themes (case-insensitive compare against the lowercase default; test covers `#22D3EE`).
- **Flows converge regardless of ordering** because `applyTheme` re-applies the accent from LS and `applyColors` keys on the live DOM class:
  - `applyPreview` (AppearanceSection.tsx:63-76): `applyTheme` → `applyColors`; the LS-sourced intermediate accent is synchronously overwritten by the draft accent. ✔
  - `resetAppearance` (useAgentStore.ts:821-833): `applyTheme("dark")` reads a possibly-stale LS accent, then `writeColorPrefs`/`applyColorPrefs` writes defaults; final DOM accent = `#22d3ee` in dark. ✔
  - App mount (App.tsx:120-131), OS scheme-change listener (App.tsx:127-129), `setTheme`/`setColor`/`resetColors`, and Settings cancel-rollback (deactivation re-applies the committed snapshot) all end in the correct effective accent. ✔
  - Store seeding `accentColor: readLs(LS_ACCENT_COLOR, DEFAULT_ACCENT_COLOR)` (useAgentStore.ts:571) matches `applyTheme`'s LS source. ✔
  - `handleResetDefaults` (AppearanceSection.tsx:158-178) and the "Default dark" preset (:277-295) match the `DEFAULT_*` constants exactly. ✔
- **WCAG math in themeContrast.test.ts** is spec-correct (0.03928 threshold, c/12.92, ((c+0.055)/1.055)^2.4, 0.2126/0.7152/0.0722, (hi+0.05)/(lo+0.05)). Independently recomputed all 8 asserted pairs — dark: text 14.5, muted 5.7, accent 9.9, contrast-text 9.9; light: text 14.6, muted 6.9, accent 5.4, contrast-text 5.4 — all pass the asserted floors (7 / 4.5 / 4.5 / 4.5). Token parser is 6-digit-hex-only and `tok()` throws on missing tokens (fail-loud). ✔
- **Tests registered** in vitest.config.ts:49-50; `node` environment + `readFileSync(new URL(...))` matches the proven scrollbar.test.ts pattern. ✔
- **README.md:60** bullet is accurate (dark default kept, system-following, automatic accent follow, CSS-parsing regression test). PLAN.md:406 ("Dark theme by default (light toggle)") is still true — no PLAN.md update needed. ✔
- **Multi-platform neutrality:** pure CSS/TS, no platform APIs. **Security:** nothing to flag. ✔
