## Verdict: PASS

Re-review of the fix for the single low finding in
`.coding/reviews/2026-08-24-md-typography-review.md`, on branch
`wt/fix-md-typography`. The main agent committed the fix as `ac510628`
("Install @tailwindcss/typography so prose classes render markdown headings");
this re-review examines that commit (`git show HEAD`), not the working tree
(the fix is already committed, so `git diff` would show nothing — per the
finish-gate re-review convention).

---

### Original finding — addressed ✓

**Finding 1 (low):** `@tailwindcss/typography` (a direct MIT-licensed
devDependency) was missing from the About dialog's dependency manifest.

**Verified fixed.** `frontend/src/components/about/dependencies.ts` now
contains, at line 144 — the first entry of the "Frontend — build & dev" group
(lines 141-156):

```ts
{ name: "@tailwindcss/typography", license: "MIT", registry: npm("@tailwindcss/typography"), licenseUrl: MIT },
```

- **Fields correct.** `license: "MIT"`, `registry: npm("@tailwindcss/typography")`
  (resolves to `https://www.npmjs.com/package/@tailwindcss/typography` via the
  existing `npm()` helper), and `licenseUrl: MIT` (the same `MIT` constant
  `https://opensource.org/licenses/MIT` used by every other MIT entry). Exact
  match to the string the original reviewer specified. ✓
- **Position correct.** Placed immediately before `@tauri-apps/cli` (line 145),
  preserving the group's alphabetical ordering: `@tailwindcss/typography` <
  `@tauri-apps/cli` (third char `i` (0x69) < `u` (0x75)). ✓
- **Scope correct.** It sits in the "Frontend — build & dev" group, which the
  manifest's own doc comment (lines 8-13) defines as the home for direct
  *devDependencies* — exactly the contract the original finding cited. ✓

The manifest is now complete: every direct dependency across the three
manifests is credited exactly once.

---

### Verified clean

- **Commit scope is exactly the four expected source changes.** `git show HEAD`
  touches: (a) `frontend/package.json` — `@tailwindcss/typography ^0.5.19`
  added under `devDependencies`; (b) `frontend/tailwind.config.ts` —
  `import typography from "@tailwindcss/typography"` + `plugins: [typography]`
  (replacing the empty `plugins: []`), license header and all other config
  (`content`, `darkMode`, `theme.extend`) preserved; (c) `package-lock.json` —
  the new entry plus its transitive `postcss-selector-parser@6.0.10`, both
  `dev: true` / `license: "MIT"`; (d) `frontend/src/components/about/dependencies.ts`
  — the one manifest entry above. ✓
- **No unintended source edits.** No `.tsx` files modified; no changes to
  `frontend/src/styles/globals.css`. The hand-written `.prose` rules
  (`.prose { font-size: inherit }`, `.prose pre { margin: 0 }`, table/list/link/
  blockquote rules) remain intact and still layer on top of the plugin, as the
  original review required. ✓
- **Plugin registration is canonical.** `import typography` + `plugins: [typography]`
  is the documented Tailwind plugin registration; it is what makes `prose` /
  `prose-invert` / `prose-sm` and the `prose-*` modifiers generate CSS. ✓
- **Peer-dependency compatibility.** Lockfile declares the plugin's peerDep as
  `tailwindcss: ">=3.0.0 || …"`; the project pins `tailwindcss ^3.4.15`, which
  satisfies `>=3.0.0`. ✓
- **devDependency placement.** Correctly under `devDependencies` in both
  `package.json` and the lockfile (`"dev": true`) — build-time only, not
  shipped to runtime. ✓
- **Build + tests.** Consistent with the main agent's report: `npm run build`
  exit 0, `npm test` 533 passed / 0 failed. The change is purely additive
  (plugin registration + a data-only manifest entry); a clean build/test run is
  the expected outcome. (Read-only reviewer did not re-run.) ✓
- **Multi-platform neutrality.** Pure CSS generation + a TypeScript data array.
  No platform-specific APIs, paths, or shell syntax; no `cfg(windows)`-only
  additions. ✓
- **Documentation sync.** No update required. `PLAN.md` and `README.md` describe
  the markdown *parsing* stack (`react-markdown + remark-gfm + rehype-highlight`);
  the typography plugin is a Tailwind *styling* concern, not part of that
  pipeline, and no prose/typography mention exists in either doc to go stale. ✓
- **Constitution.** No `#[allow(...)]` (N/A — frontend). License header comment
  in `tailwind.config.ts` preserved; SPDX header in `dependencies.ts`
  preserved. ✓

No new findings. The fix is correct, complete, and introduces no regressions.
