## Verdict: FINDINGS (0 high, 1 low)

Review of uncommitted changes on `wt/fix-md-typography` (3 files: `frontend/package.json`, `frontend/tailwind.config.ts`, `package-lock.json`). The change registers the `@tailwindcss/typography` Tailwind plugin so the `prose` classes already present in 5 markdown surfaces generate CSS.

---

### Finding 1 (low) — New direct devDependency missing from the About-dialog manifest

**File:** `frontend/src/components/about/dependencies.ts` (the "Frontend — build & dev" group, lines 141-154)

`@tailwindcss/typography` is a **direct** devDependency (added to `frontend/package.json` devDependencies) and is **MIT-licensed** (confirmed in `package-lock.json`: `"license": "MIT"`). The `dependencies.ts` manifest's own doc comment defines its scope as:

> "Lists the DIRECT dependencies of Mnemo across three manifests … Transitive dependencies are intentionally omitted: crediting the direct deps (whose licenses we rely on) is the scope of the About dialog." (lines 8-13)

The file already maintains a dedicated **"Frontend — build & dev"** group (lines 141-154) that credits direct *devDependencies* — `tailwindcss`, `autoprefixer`, `postcss`, `@vitejs/plugin-react`, `vite`, `vitest`, `typescript`, `@types/react`, `@types/react-dom`, `@tauri-apps/cli`. The review brief's premise that "`dependencies.ts` lists runtime deps … this is a build-time devDependency — correctly NOT added" is factually wrong: the file explicitly credits build/dev direct deps, and `@tailwindcss/typography` is one. By the manifest's own stated contract it belongs in that group.

**Impact:** The About dialog's license-attribution manifest is now incomplete — a direct MIT-licensed dependency is uncredited. Low severity: MIT is permissive, the package is build-time only (not shipped to runtime), and the omission carries no compliance risk. But it is a real documentation-sync gap introduced by this change.

**Fix** — add one entry to the "Frontend — build & dev" group (alphabetically, before `@tauri-apps/cli`):

```ts
{ name: "@tailwindcss/typography", license: "MIT", registry: npm("@tailwindcss/typography"), licenseUrl: MIT },
```

---

### Verified clean

- **Fix correctness.** Registering the plugin via `import typography from "@tailwindcss/typography"` + `plugins: [typography]` (`tailwind.config.ts:6,25`) is the canonical Tailwind plugin registration; it is what makes `prose` / `prose-invert` / `prose-sm` and the `prose-*` modifiers generate CSS. Without it, `prose` produced zero CSS and Tailwind Preflight reset headings to `font-size: inherit; font-weight: inherit` — the reported symptom. ✓
- **Peer-dependency compatibility.** `package-lock.json` declares the plugin's peerDependencies as `"tailwindcss": ">=3.0.0 || insiders || >=4.0.0-alpha.20 || >=4.0.0-beta.1"`. The project pins `tailwindcss ^3.4.15` (`frontend/package.json:36`), which satisfies `>=3.0.0`. ✓
- **devDependency placement.** Correctly under `devDependencies` (`frontend/package.json:28`) and `"dev": true` in the lockfile — Tailwind plugins run at build time only and are not shipped to runtime. ✓
- **No unintended source edits.** `git diff HEAD` touches only `package.json`, `tailwind.config.ts`, `package-lock.json`. No `.tsx` files modified. The `prose prose-invert prose-sm` classes (and modifiers `prose-pre:*`, `prose-h1:*`, `prose-h2:*`) were confirmed pre-existing at `Message.tsx:214`, `SourceEditor.tsx:513/519/525`, `PlanProgress.tsx:229`. ✓
- **`globals.css` untouched / hand-written `.prose` rules intact.** No changes to `frontend/src/styles/globals.css`. The layering is correct and must remain: `.prose { font-size: inherit }` (line 234) overrides `prose-sm`'s 0.875rem so the user's `--app-font-size` applies; `.prose pre { margin: 0 }` (line 230), table/list/link/blockquote rules (lines 242-248) all still present and still needed on top of the plugin. ✓
- **License.** `@tailwindcss/typography` is MIT (lockfile). Its transitive dep `postcss-selector-parser@6.0.10` is also MIT. ✓
- **Multi-platform neutrality.** Pure CSS generation — no platform-specific APIs, paths, or shell syntax. No `cfg(windows)`-only additions. ✓
- **Build + tests.** Consistent with the main agent's report (`npm run build` exit 0, CSS bundle now includes prose styles; `npm test` 533 passed / 0 failed). The change is purely additive plugin registration; a clean build is the expected outcome. (Read-only reviewer did not re-run.) ✓
- **Constitution.** No `#[allow(...)]` (N/A — frontend). License header comment in `tailwind.config.ts` preserved. ✓
- **Documentation sync (README.md / PLAN.md).** No update required. `PLAN.md` (lines 127, 558, 620, 676) and `README.md` describe the markdown *parsing* stack (`react-markdown + remark-gfm + rehype-highlight`); the typography plugin is a Tailwind *styling* concern, not part of the parsing pipeline. No prose/typography mention exists in either doc to go stale. ✓

---

### Recommended actions

1. **(Required — Finding 1)** Add the `@tailwindcss/typography` entry to the "Frontend — build & dev" group in `frontend/src/components/about/dependencies.ts`.
2. Re-run `npm run build` + `npm test` after the `dependencies.ts` edit (trivial data-only change; expected green).
3. Commit to `wt/fix-md-typography` including this review report, then `finish`.
