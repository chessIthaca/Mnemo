## Verdict: FINDINGS (0 high, 2 low)

Implementation review of plan 969510fc "Project open/create progress feedback (486955d5)" — all uncommitted changes on wt/agenticcoding (6 modified + 5 untracked files; frontend-only source plus `.coding/` bookkeeping). The change is correct and safe: the App early return sits after every hook and breaks no existing gate or test, all busy-reset paths carried over to `phase` (no stuck-busy state), the CSP permits the inline splash styles, and both new test files are correctly registered (and guarded by the vitestInclude allow-list test). Two LOW findings: small visual deltas between the static splash and its React "twin" undercut the stated seamless handoff, and the two behavioral wirings (App early return, ProjectPicker phase walk) have no regression guard despite the repo's source-contract test pattern.

### Findings

**LOW 1 — Static splash / BootSplash visual parity deltas (the "seamless handoff" goal).**
Files: `frontend/index.html`, `frontend/src/components/common/BootSplash.tsx`.
The colors match exactly (`#0f172a` = the dark `--bg-primary`, globals.css:11; `#e2e8f0` = slate-200; `#334155` = slate-700; `#22d3ee` = cyan-400; `#94a3b8` = slate-400), but five numeric values differ between the static splash and its React twin:

| element | static splash | BootSplash |
|---|---|---|
| wordmark size | `1.6rem` | `text-2xl` = 1.5rem |
| letter-spacing | `0.12em` | `tracking-widest` = 0.1em |
| spinner size | `22px` | `h-5 w-5` = 20px |
| spin duration | `0.9s` | `animate-spin` = 1s |
| "Starting…" size | `0.8rem` | `text-sm` = 0.875rem |

The plan, the BootSplash doc comment, and the new README clause all claim a "matching"/"seamless" handoff; these deltas produce a small but perceptible jump (wordmark shrinks ~7%, spinner shrinks and slows) at exactly the moment the change is supposed to make invisible. Fix in either direction: align the static values to the Tailwind utilities (1.5rem / 0.1em / 20px / 1s / 0.875rem), or use arbitrary-value classes in BootSplash (`text-[1.6rem] tracking-[0.12em] h-[22px] w-[22px] text-[0.8rem]` + `[animation-duration:0.9s]`).

**LOW 2 — No regression guard on the two behavioral wirings.**
Files: `frontend/src/App.tsx`, `frontend/src/components/projects/ProjectPicker.tsx`.
The new tests cover the pure label mapping and the BootSplash render, but the actual behavior changes — the `if (!checkedStartup) return <BootSplash />;` placement (before the picker/error returns, after all hooks) and the handleCreate phase walk (`preparing` → `restarting`, reset-to-null on every error path) — are pinned by nothing. A future refactor that moves the early return below the picker return, or drops a `setPhase(null)` from a catch, silently regresses the boot splash / re-introduces a stuck-busy state with no test failing. The repo's established pattern for node-env-untestable component wiring is the source-contract test (`App.shellRender.test.ts`, `InflightBar.test.ts`): a few `expect(source).toContain(...)` assertions (the early return before the picker return; `setPhase("preparing")` before `createProject`; `setPhase("restarting")` before `switchProject`; `setPhase(null)` in each catch/finally) would close the gap cheaply and in-convention — e.g. as a new block in `BootSplash.test.tsx` / a small `projectPickerPhases` companion, or an addition to `App.shellRender.test.ts`.

### Requested checks — evidence

**1. App.tsx early-return placement — no existing behavior broken.**
- *Hooks order:* the `if (!checkedStartup) return <BootSplash />;` (App.tsx:595) sits after the LAST hook in App (the window-geometry `useEffect` ending at :589); no hook follows it in the component body (`ResizeHandle` is a separate function). All startup machinery still runs during the splash: `useAgentEvents`, the reconcile-event subscription, the startup effect (`getNeedsProject` → `getStartupError` → snapshot/settings/safety-mode), the embedder poll, the git-branch poll, and the geometry restore — all declared above the return.
- *Gates:* the `checkedStartup && needsProject` picker return (:603) and `checkedStartup && startupError` screen (:608) are unchanged and now trivially redundant (checkedStartup is always true past the splash) — harmless, pre-existing style. Ordering (picker before error screen) preserved. The needs-project path sets `checkedStartup` at :206 before returning from the check, so the splash → picker transition is direct.
- *App.shellRender.test.ts:* source-contract based — asserts `appSource` does NOT contain `s.agents[s.activeAgent]` (still true) and DOES contain `<InflightBar agentId={activeAgent} />` (still present at :726). Unaffected.
- *IndexingOverlay two-copy invariant:* the App-tree copy now mounts only after `checkedStartup` resolves; a startup index pass that began during `build_brain` is caught up via the `get_index_progress` snapshot — exactly the documented late-mounting catch-up path (README: "a late-mounting overlay catches up via the `get_index_progress` snapshot"). The startup-mode ProjectPicker copy is unchanged.
- *Comment accuracy:* `build_brain` does run in Tauri's sync `.setup` hook on the main thread (src-tauri/src/main.rs:288, doc at :978) before IPC is managed, so the new comment's claim is correct.
- *Failure mode:* if the startup IPC never resolves, the app previously showed the empty main-UI shell forever; it now shows "Starting…" forever — equivalent, better messaging.

**2. ProjectPicker phase wiring — complete, no stuck-busy state.**
- `handleOpen`: `setPhase("restarting")` → catch resets to null; success is unreachable (process restarts) — matches the old `busy` semantics exactly.
- `handleRemove`: `setPhase("removing")` → `finally` resets on BOTH success and error (the only finally-reset, correctly on the flow that returns normally).
- `handleCreate`: guard returns before any phase is set; `setPhase("preparing")` → `createProject` → `setPhase("restarting")` → `switchProject`; catch resets to null. Every path that previously reset `busy` now resets `phase`.
- `const busy = phase !== null` preserves the `disabled={busy}` semantics on all four buttons (Open / Remove / Choose directory… / Create & open) — no enable/disable drift.
- Design note (not a finding): `phase` is global, so during a restart every row's Open button reads "Restarting…" — consistent with the all-disabled state and documented in `openButtonLabel`'s doc comment; same information shape as the old all-disabled "Working…".

**3. Static splash — sound.**
- Inline styles only on the splash div + one inline `<style>` in `<head>` (keyframes + `.mnemo-boot-spinner`); zero CSS-bundle dependency. The namespaced `mnemo-boot-*` names collide with nothing.
- CSP: both `csp` and `devCsp` in src-tauri/tauri.conf.json carry `style-src 'self' 'unsafe-inline'` — the inline `<style>` and style attributes are permitted in dev AND production.
- React replacement: `ReactDOM.createRoot(document.getElementById("root")!).render(...)` (main.tsx:20) — React 18 `createRoot().render()` replaces existing container children on initial mount; `main.tsx` is the only code touching `#root` (verified: sole `getElementById` hit), so no leftover splash markup and no other consumer of `#root` emptiness.
- Dark-only decision documented in the HTML comment; `--bg-primary: #0f172a` (dark) confirmed at globals.css:11 with the `#ffffff` light override at :46 — the comment's rationale (pre-JS splash cannot know the user's light-mode setting) is accurate.

**4. vitest include registration — correct.**
- `src/components/common/BootSplash.test.tsx` (vitest.config.ts:26) and `src/components/projects/projectPickerPhases.test.ts` (:86) both match the on-disk paths exactly (case-sensitive, correct extensions, correct directories).
- The `src/lib/vitestInclude.test.ts` guard fails loudly on any unregistered test file under src/, so the reported green run (81 files / 1102 tests, including the 2 new files) proves registration is complete and the files actually executed.

**5. Multi-platform neutrality — clean.** Frontend-only change: standard HTML/CSS (inline styles, one keyframes animation), React/Tailwind classes, pure TS label functions. No platform-specific APIs, paths, or shell syntax; renders identically in WebView2 (Windows) and WKWebView (macOS).

**6. Docs sync — accurate and well-placed.**
- README: the new clause is appended to the existing open/create/switch UX sentence inside the content-index bullet (where the IndexingOverlay behavior is already documented) — consistent placement. Content is accurate: static pre-React splash (inline-styled, dark, wordmark + spinner, first paint → React mount), BootSplash over the pre-index boot phases, phase-aware button labels replacing "Working…". (The word "matching" is LOW 1's only stretch.)
- Module doc comments: present and accurate on `BootSplash.tsx` (purpose, twin relationship, why text wordmark not MnemoLogo) and `projectPickerPhases.ts` (type + both exported functions, including the unreachable-restart note). The test files follow the existing convention (copyright header + intent-bearing describe/it, cf. MnemoLogo.test.tsx).
- No stale "Working…" references remain in code (only the two explanatory comments).

**7. Test coverage — meaningful for the exported logic.**
- `projectPickerPhases.test.ts`: the full mapping table — all four `createButtonLabel` arms (null/preparing/restarting/removing) and all four `openButtonLabel` arms, including the "only restarting changes Open" contract.
- `BootSplash.test.tsx`: `renderToStaticMarkup` asserts the wordmark, "Starting…", the full-screen classes (`h-screen w-screen`, `bg-bg-primary`), and the spinner (`animate-spin`) — the repo's established node-env render-test pattern (cf. MnemoLogo.test.tsx).
- Gap → LOW 2: the wiring (App early return, ProjectPicker phase walk) is untested.

### Other review dimensions

- **Security:** no new inputs, no dynamic HTML, no injection surface — the splash is static markup and the labels are constant strings. Clean.
- **`.coding/backlog.jsonl`:** the diff is benign app-managed bookkeeping — item 486955d5 flipped pending → in_flight (this plan's dispatch), plus two documented missed-auto-done-flips (5a85e36c, da188979) with explanatory notes; no item text was altered.
- **Constitution:** file-tools policy respected (no shell mutation in the diff); code style matches the repo; the frontend suites + `tsc --noEmit` were run as the project's tests for this frontend-only change (no Rust surface touched — nothing for `cargo test` to cover); no `#[allow]`/warning surface involved.
- **Verification reliance:** tests were not re-run by this reviewer (read-only); the report relies on the parent's reported green run, cross-checked against the vitestInclude guard's semantics.
