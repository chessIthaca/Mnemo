## Verdict: FINDINGS (0 high, 2 low)

**Plan goal:** the InflightBar reasoning/activity panel's open/closed state is the USER's choice alone (chevron click or a drag on the handle), persisted across restarts, defaulting to CLOSED — no code path may expand it.

**One-line summary:** the fix is correct and complete — the auto-open edge is gone, `expanded` has exactly one writer, the pref defaults closed under every guard, docs/comments/tests are coherent and multi-platform neutral; two Low findings concern a regression-test assertion that under-delivers on its own title and one imprecise word in the new feature doc.

**Reviewed (all uncommitted, `git_read op="diff"`):** `frontend/src/components/chat/InflightBar.tsx`, `frontend/src/components/chat/InflightBar.test.ts`, `frontend/src/lib/inflightPanelPref.ts` (new), `frontend/src/lib/inflightPanelPref.test.ts` (new), `frontend/src/lib/reasoningPanel.ts` + `.test.ts` (deleted), `frontend/vitest.config.ts`, `docs/FEATURES.md`. `.coding/**` bookkeeping was excluded per the task.

### Focus-point verification

**1. No auto-open path remains — PASS.**
- Repo-wide search for `reasoningActive|reasoningBlockStarted|prevReasoningActive|reasoningPanel` returns only: the intentional `not.toContain` strings + doc comment in `InflightBar.test.ts:166,171-173`; the new comments that narrate the *removed* behavior (`InflightBar.tsx:88-92`, `inflightPanelPref.ts:8-12`, `inflightPanelPref.test.ts:9-12`); and `.coding/` history. **Zero hits under `src-tauri/`** and none elsewhere in `frontend/src`.
- `expanded` has exactly one writer: `frontend/src/components/chat/InflightBar.tsx:104-107` (`setPanel` = `setExpanded(open)` + `writeInflightPanelOpen(open)`). `setExpanded(` occurs **once** in the file (`:105`); `:93` is the `useState` destructure (`setExpanded]`). All other `setExpanded(` hits in the repo belong to unrelated components (`Message.tsx`, `ProvidersSection.tsx`, `FileViewer.tsx`, `GitView.tsx`, `LlmTraceView.tsx`, `MemoryDebugView.tsx`, `BacklogView.tsx`).
- The component's three remaining effects never touch `expanded`: elapsed timer `:112-118` (deps `[running]`), auto-scroll `:121-124` (deps `[activityLog]`), drag listeners `:175-195` (deps `[]`). Nothing keys off `phase` or turn lifecycle events for expansion any more.
- StrictMode double-invoke is harmless: the only mount-time work is the pure `readInflightPanelOpen()` read. Agent switching cannot flip it either — `InflightBar` is mounted once (`frontend/src/App.tsx:760`) and the inner panel instance is reused across `agentId` changes (no `key`); when the store slice disappears the outer returns null (`:71`) and the remount re-reads the persisted value, which equals the last user choice.

**2. Default CLOSED for every non-`"1"` value, guards never throw — PASS.**
- `parsePanelOpen` (`inflightPanelPref.ts:27-29`) is `raw === "1"`; the table at `inflightPanelPref.test.ts:37-49` covers `null`, `""`, `"0"`, `"true"`, `"open"`, `"yes"`, `"2"`, `"-1"`.
- `readInflightPanelOpen` (`:31-39`) checks `typeof window === "undefined"` first, then try/catch; `writeInflightPanelOpen` (`:41-49`) no-ops without `window` and swallows `setItem` throws. `frontend/vitest.config.ts:9` runs `environment: "node"`, and the test proves both degradations (`inflightPanelPref.test.ts:69-73` no window, `:75-88` throwing storage).
- `useState(readInflightPanelOpen)` (`InflightBar.tsx:93`) passes the function **reference** → React's lazy initializer (an immediate call would read `useState(readInflightPanelOpen())`). Correct, and pinned by `InflightBar.test.ts:177`.

**3. Every user toggle persists; nothing else regressed — PASS on code (test pinning weak: finding L1).**
- Chevron/bar click: `InflightBar.tsx:252` → `setPanel(!expanded)` → persist. Drag handle collapsed branch: `:200-202` `if (!expanded) { setPanel(true); }` → persist (an already-open panel only resizes, so no write is needed).
- Drag-to-resize itself is unchanged: `startDrag` `:197-207` (preventDefault, cursor/userSelect, `dragging.current`), move/up listeners `:175-195` (`rect.bottom - e.clientY`, `MIN_HEIGHT`/`MAX_HEIGHT` clamp `:182`), handle JSX `:236-246` (grip pill, `border-y`, `cursor-ns-resize`, `title="Drag to resize"`).
- `aria-expanded={expanded}` `:253`, `aria-label="Toggle reasoning panel"` `:254`, chevron render `:287-293`, auto-scroll `:121-124`: all unchanged.

**4. Other `phase`/`activityLog` consumers intact — PASS.** `phaseLabel` `:31-54` / call site `:270`; `phaseLabelClass` `:57-59` / `:269`; `hasActivity` `:170` used at `:216` (showBar) and `:495` (entry gate); the token stats row `:298-345` (↑/↓/🧠, rolling tok/s + tooltip) untouched; elapsed timer `:112-118`/`:272-276`; ctx bar + Compact popup `:350-476`; `ActivityLine` `:506-522`. The deletion removed exactly the `reasoningNow` const, the `prevReasoningActive` ref and the effect — `phase` (`:270`) and `activityLog` (`:124`,`:170`,`:495`) are still referenced and `useEffect` (`:5`) is still needed by three effects, so no dead symbol was left behind.

**5. Docs/comment truth — PASS except one wording nit (finding L2).** `docs/FEATURES.md:49` was rewritten to the new behavior. Grep for `auto-open|auto-opens|auto-expand|opens itself|opened itself|expands itself` finds, besides that line, only comments that explicitly describe the removed effect and unrelated features (ConfigDialog endpoint auto-expand, PlanProgress active-step auto-expand, `browser_navigate`). `README.md` / `PLAN.md` / `docs/**` contain no inflight-bar or reasoning-box claim other than `docs/FEATURES.md:49`. Memory hygiene checks out: the new DECISION record (2aba084b) states the user-controlled behavior, and a semantic search for the old auto-open claim surfaces only historical PLAN/BUG/SPEC records about adjacent behavior — no live behavioural record contradicts the fix.

**6. Regression-test quality — PASS for reproduction, one case under-asserts (finding L1).** The pre-fix source (from `git diff HEAD`) imported `reasoningActive`/`reasoningBlockStarted`, held `prevReasoningActive` and used `useState(false)` — so all three cases at `InflightBar.test.ts:170-182` fail on it: a genuine reproduction, not a tautology. Registration is sound: `vitest.config.ts:65` adds the new pref test, the deleted module's entry is gone, and the filesystem-driven guard `frontend/src/lib/vitestInclude.test.ts:79-97` fails if any `*.test.ts(x)` under `src/` is unregistered — so the new file provably executes.

**7. Multi-platform neutrality — PASS.** Pure TS/React + Markdown docs; no OS paths, APIs, shell syntax or new dependency (`frontend/package.json` untouched); `window.localStorage` is standard in WebView2 (Windows) and WKWebView (macOS).

**8. Project constitution — PASS.** Doc comments on every new public symbol (`inflightPanelPref.ts:19,22-26,31,41`) and on the local `setPanel` helper (`InflightBar.tsx:99-103`). No `#[allow]`, no Rust surface touched at all, so `#![deny(warnings)]` is not at risk. The two deletions used `Remove-Item`: the file tools expose no delete primitive (`file_edit` needs an existing anchor, `file_write` only creates/overwrites), so that fallback justification holds.

**Residual uncertainty:** a reviewer subagent has no shell tool, so I could not independently re-run `npm test`, `npm run build` or `cargo test`; I verified static consistency (no dangling import, coherent include list, guard test would catch a missing registration) and rely on the implementer's reported green runs. Re-run all three after fixing the findings, before commit.

### Findings

#### L1 (Low) — The drag-handle persistence is not actually pinned; the new test case under-delivers on its own title

**Evidence.** `frontend/src/components/chat/InflightBar.test.ts:180-182` asserts only

```ts
it("persists every user toggle (chevron click and drag handle)", () => {
  expect(source).toContain("writeInflightPanelOpen(");
});
```

That substring is satisfied by the import and the helper definition alone (`InflightBar.tsx:10`, `:106`). Concretely: reverting `startDrag`'s collapsed branch (`InflightBar.tsx:200-202`) from `setPanel(true)` back to `setExpanded(true)` — a real user-visible regression, where a drag opens the panel for the session but a restart silently loses the choice — leaves **every** test in the repo green. The chevron path is at least pinned by the older assertion at `InflightBar.test.ts:57` (`onClick={() => setPanel(!expanded)}`); the drag path has no equivalent, and the "no auto-open wiring" case (`:170-174`) only forbids the deleted module's *names*, so a future auto-open written as `setExpanded(true)` would also slip through.

**Suggested fix** (pin the call sites and the single-writer invariant):

```ts
it("persists every user toggle (chevron click and drag handle)", () => {
  expect(source).toContain("onClick={() => setPanel(!expanded)}");   // chevron/bar click
  expect(source).toContain("setPanel(true)");                        // startDrag collapsed branch
  expect(source).not.toContain("setExpanded(true)");                 // no auto-open bypassing setPanel
  expect(source.match(/setExpanded\(/g) ?? []).toHaveLength(1);      // setPanel is the ONLY writer
});
```

(The count is 1 today — `:93` is the destructure `[expanded, setExpanded]`, which does not match `setExpanded(`. Adjust the count if a future refactor legitimately adds a second writer.)

#### L2 (Low) — `docs/FEATURES.md:49` overstates the drag handle as a *toggle*

**Evidence.** The new text reads "…only the chevron click or a drag on the handle toggles it; the choice is remembered across restarts…", but `startDrag` (`frontend/src/components/chat/InflightBar.tsx:197-207`) only ever opens: `if (!expanded) setPanel(true)` — dragging an already-open panel resizes it and can never close it. A reader (or a future implementer) can take "the handle toggles it" as licence to make the handle close the panel too, which would contradict the stated behaviour and the persisted-choice semantics.

**Suggested fix.** "…it never opens itself — only the chevron click (or starting a drag on the handle, which opens it) changes it; the choice is remembered across restarts and a fresh install starts collapsed…"

### Observations (no action required)

- **Two-instance sync** — the persisted flag is read at mount only (no `storage` event listener), so two simultaneously mounted bars would not reflect each other's toggle live. `InflightBar` is mounted exactly once (`frontend/src/App.tsx:760`), so this is theoretical; noted only because the pref is global rather than per-agent (which is consistent with the single mount and the docs).
- **Memory/bookkeeping** — the DECISION record (2aba084b) matches the shipped behaviour. The 2026-08-22 PLAN record for the auto-open feature remains as a dated historical artifact (like the plan files under `.coding/plans/`), not a live behavioural claim; no supersession needed. `.coding/knowledge/bug/506b85e2.md` (`status = "superseded"`) is unrelated side-car bookkeeping and was not reviewed as code, per the task.

### Verification method + residual uncertainty

- Evidence gathered by reading the complete uncommitted diff (`git_read op="diff"`, non-truncating), all changed/new files in full, the deleted module's pre-fix content from the diff, `frontend/vitest.config.ts`, `frontend/src/lib/vitestInclude.test.ts`, `frontend/tsconfig.json`/`package.json`, and repo-wide searches for every identifier and doc phrase the change touches.
- A reviewer subagent has no shell tool, so `npm test`, `npm run build` and `cargo test` were **not** independently re-run; static consistency (no dangling import, coherent vitest include list, the include-guard test would catch a missing registration, no unused symbols) was verified instead, and the implementer's reported runs (frontend 82 files / 1112 tests, build exit 0; root `cargo test` 2338 passed) are taken at face value. Re-run all three after applying L1/L2, before commit.
- Neither finding questions the fix's correctness: the auto-open path is gone, the only writer is `setPanel`, the default is closed under all guards, and the user-facing behaviour matches the requested design.
