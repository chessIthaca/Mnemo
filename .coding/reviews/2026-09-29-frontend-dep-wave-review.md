## Verdict: FINDINGS (0 high, 4 low)

Frontend review of the merged dependency-modernization wave, committed range `d158fb7..HEAD` (PRs #16–#60). The working tree is clean, so the review target is the committed delta: React 19 (d8ea862), TS 7.0.2 (1d94c42), Tailwind 3.4→4.3 (d56abd6), zustand 5.0.15 (7c9dce7), react-markdown 10.1 (8a7c7cf), lucide-react 1.48 (162c444), vitest 5 (e40d3e1), plus fixes a55ed24 (#185 render loop) and f3dd412 (nested button / Radix Tabs). Read-only review: commits via git log/show, current state via file reads and targeted searches; no builds/tests were run (findings below are all doc/metadata level — nothing indicates a runtime defect).

### What I actually read

Commit diffs: a55ed24, f3dd412, d8ea862, 7c9dce7, 8a7c7cf, 162c444, e40d3e1, 1d94c42 (full), d56abd6 (stat + config/CSS portions). Current files: `frontend/package.json`, `frontend/postcss.config.js`, `frontend/src/styles/globals.css`, `frontend/src/hooks/useAgentStore.ts` (head + create call at :680), `frontend/src/components/chat/InflightBar.test.ts` (all 265 lines), `frontend/src/components/chat/InflightBar.tsx` (button occurrences), `frontend/src/components/chat/MarkdownImpl.tsx`, `frontend/vitest.config.ts`, `frontend/src/components/about/dependencies.ts` (all 187 lines), `README.md`/`PLAN.md` version references, plus every `useAgentStore(` selector call site (~100 across 22 files) and every `lucide-react` import line (48 files).

### Criterion verification

**1. React #185 root cause + fix — VERIFIED CLEAN.** `useAgentStore.ts:23` imports `createWithEqualityFn as create` from `zustand/traditional`, invoked at `:680` (`create<AppState>((set, get) => …)`, default `Object.is`). The claim is technically sound: `useSyncExternalStoreWithSelector` caches the selector result against the *store snapshot identity* (it only re-runs the selector when the store state object changes), so `createWithEqualityFn` restores v4's per-consumer memoization for every selector at once — no per-selector audit required. I audited anyway: all ~100 `useAgentStore(` call sites return either store-held references (`s.agents[id]`, `s.workflowStates[mainId] ?? null`, actions, maps), primitives (`selectMainAgentId` → id, `GitView.tsx:249-251` reduce → number), or — the only fresh-composite selector — `MainPanel.tsx:48-49`, which is wrapped in `useShallow` (imported from `zustand/react/shallow`, `MainPanel.tsx:7` — valid v5 path). No remaining fresh-object/fresh-array selector reaches the store hook unguarded. The store-hook comment block (`useAgentStore.ts:14-22`) documents the root cause accurately — good doc sync.

**2. InflightBar.test.ts regression value — VERIFIED CLEAN.** The suite (`InflightBar.test.ts:218-240`) pins structure, not text: `expect(source.match(/<button/g) ?? []).toHaveLength(1)` on the component source imported via `?raw` (`:23`); the pre-fix component had two `<button` elements (toggle + Compact control), so this count fails without the fix. It also pins the replacement shape: `role="button"` span, `aria-label="Compact context"`, Enter/Space handling, `tabIndex={running || agentId === null ? -1 : 0}`, and `aria-disabled={…}` with a lookbehind regex (`:236`) that would fail if the bare `disabled={running || agentId === null}` attribute returned. Current `InflightBar.tsx` has exactly one `<button` (line 273, closing 563) and one `role="button"` (line 529) — consistent.

**3. Tailwind 4 migration — VERIFIED CLEAN.** `postcss.config.js:5-9` wires only `'@tailwindcss/postcss': {}` (autoprefixer also gone from `package.json`). `globals.css:1-5` uses `@import 'tailwindcss'`, `@plugin '@tailwindcss/typography'`, and `@custom-variant dark (&:is(.dark *))` (the v4 replacement for `darkMode: 'class'`); `tailwind.config.ts` is deleted (no references remain anywhere in frontend/src). The v4 default-border-color change is shimmed by the upgrade tool's standard `@layer base` compat block (`globals.css:23-31`). Searches across frontend/src found zero leftover v3-isms: no `@tailwind base/components/utilities` directives, no `bg-opacity-*`/`text-opacity-*`/`flex-shrink`/`overflow-ellipsis`/`outline-none`, no bare `ring`, no `shadow-sm`/`shadow-xs`, no `space-x-`/`divide-x`/`blur-sm` (the one `shadow-sm` hit, `ClassifierSection.tsx:460`, is prose text, not a class). The hand-written `!important` cyan-override rules (`globals.css:94-103`) define their own selectors and survive v4 class generation unchanged.

**4. Breaking-change fallout — VERIFIED CLEAN (static; builds not re-run).**
- *react-markdown 10*: usage is limited to `remarkPlugins`/`rehypePlugins`/`components` + children-as-string (`MarkdownImpl.tsx:48-56`) — all still the v10 API. The `code`/component spreads already drop `node`/`ref`/`key` (`Message.tsx:312`, `SourceEditor.tsx:537`), matching React 19 ref-as-prop and react-markdown 10's extra `node` prop.
- *zustand 5*: covered under criterion 1; `zustand/react/shallow` and `zustand/traditional` are both valid v5 entry points.
- *vitest 5*: `vitest.config.ts` uses only `environment: "node"` + `include` — stable across majors; tests use `vi.hoisted` (still current API).
- *lucide-react 1.48*: all 48 files import named icons; every name seen is a current lucide export. Mixed old-alias/new-name usage exists (`AlertTriangle` in App.tsx/StatusBar.tsx/etc. vs `TriangleAlert` in LlmTraceView.tsx:6) — cosmetic inconsistency only, not a defect (and `tsc` gates it: the build script runs `tsc` first). Caveat: I could not run tsc/vitest; the commit messages record green builds and 92 files/1314 tests passing, and static inspection contradicts nothing.
- *React 19 / TS 7*: no removed-API usage found in frontend/src.

**5. dependencies.ts completeness — ONE GAP (finding L1).** Runtime group (`dependencies.ts:154-167`): all 14 `package.json` dependencies credited exactly once, correct group, no removed deps (autoprefixer absent), new deps present (`use-sync-external-store` at :165, `@tailwindcss/postcss` at :173). License labels check out (TypeScript Apache-2.0 :181, lucide ISC :158, d3-force ISC :167). Build & dev group (`:172-185`): 12 of 13 — `@types/use-sync-external-store` (added by a55ed24, `package.json:37`) is missing.

### Findings (all low)

- **L1 — dependencies.ts omits `@types/use-sync-external-store`.** `frontend/src/components/about/dependencies.ts:171-185` credits every other devDependency including all other `@types/*` (`@types/d3-force` at :184), but `@types/use-sync-external-store` (`frontend/package.json:37`, added with the #185 fix) is absent. Incomplete About-dialog attribution for a direct manifest dependency.
- **L2 — Stale version references in top-level docs left by the wave.** `README.md:84` ("React 18 + TypeScript + Vite + Tailwind UI"), `PLAN.md:24` and `PLAN.md:354` ("React 18"), and `PLAN.md:355` ("Tailwind CSS v3 (v3 toolchain: `autoprefixer` + `postcss`) … v4 is a possible future upgrade") all contradict the shipped React 19 / Tailwind 4 / no-autoprefixer state. Documentation-sync miss (may be in hand via the parent docs-sync plan — reported for completeness).
- **L3 — Stale "React 18" comment inside frontend/src.** `frontend/src/components/views/BacklogView.tsx:123` ("a synchronous setSelectionRange runs before the commit — React 18 batches the state update…"). The batching behavior it explains still holds under React 19, but the comment names the superseded version as if current; s/React 18/React/ or "React's automatic batching" would keep it accurate.
- **L4 — Mojibake em-dashes in dependencies.ts group titles.** `dependencies.ts:89` ("Rust â€” library"), `:133`, `:152`, `:171` contain `â€”` (UTF-8 em-dash mis-decoded/re-encoded) while `:143` ("Rust — vendored patches") is clean — the About dialog renders four garbled group headings. File is inside the wave's edit set; fix to `—` for consistency.

### Constitution checks

- *Documentation sync*: L2/L3 above; otherwise the wave's own docs are exemplary (useAgentStore.ts:14-22 root-cause comment; InflightBar.test.ts:200-217 fix rationale).
- *Multi-platform neutrality*: no Windows-only APIs/paths in the frontend diff — clean.
- *File-tools-first*: no shell-mutation patterns in reviewed code — N/A/clean.
- *Warning-free build*: not re-runnable here; no `#[allow]`-style suppressions or `@ts-ignore`/`eslint-disable` added — clean.
- *`.coding/**` accuracy*: the CDP-console knowledge doc referenced by f3dd412 exists and matches the described workflow (00c9b95) — accurate.

### Recommended follow-ups

Fix L1 (add the missing `@types/use-sync-external-store` entry) and L4 (encoding repair) in one small commit; fold L2/L3 into the parent docs-sync step.

Reviewed-state: 06fe9ee65aa256002d8b37d230e0c5fa51dabf2e
