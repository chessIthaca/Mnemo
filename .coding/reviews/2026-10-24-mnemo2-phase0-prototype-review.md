## Verdict: FINDINGS (2 high, 5 low)

Review of ALL uncommitted changes on `wt/mnemo` for plan 91755fa7 (Mnemo2 Phase 0 prototype). Scope read: every changed/untracked `mnemo2/**` source file (App, store, mock, review, markdown, all 14 components, all 6 test files), README.md, mnemo2/README.md, index.html, package.json scripts, and the `.coding/**` changes for accuracy only. No browsers or smoke scripts were launched.

Summary: the mock-only boundary, merge gating, responsive workbench fix, and docs are sound; two state-correctness defects ship in the primary review flows (the per-worker Diff button shows Ana's diff for all 10 workers; the fixer simulation accepts completing any open finding, writing a false review record), plus five smaller a11y/robustness gaps. Details follow.

Reviewed-state: bf44b0ef493569bdeb0f621989f82700fd3e1d5c
## High findings

### H1. Every worker's "Diff" button opens a Diff tab hard-wired to Ana — wrong data in the primary review flow

- `mnemo2/src/components/WorkerDetail.svelte:56` — the hero action `onclick={() => actions.openWorkbench("diff")}` is rendered for **every** worker and passes no worker context.
- `mnemo2/src/lib/store.ts:141-149` — `openWorkbench(tab)` records only the tab, never the worker it was opened from.
- `mnemo2/src/components/DiffTab.svelte:5` — `const ana = $derived($app.workers.find((worker) => worker.id === "ana"));`; line 20 renders the header `{ana?.branch ?? "wt/agent-ana"}`; line 24 renders the global `$app.diffFiles`, whose mock content is Ana's MergeCard work (`mnemo2/src/lib/mock.ts:519`).

Effect: from Theo (conflict), Milo (fixing), or any of the other 9 workers, clicking Diff shows "REVIEWED TIP — wt/agent-ana → wt/mnemo" with Ana's diff. A per-worker action presenting a different worker's branch/diff is exactly the interaction-model defect the prototype exists to catch. `Workbench.test.ts:18` currently bakes this in (asserts the global diff file regardless of selection).

Fix (choose one direction, implement fully):
1. Scope the tab to the worker: extend `openWorkbench` to accept a worker id (store as `workbenchDiffWorkerId`, default to `selectedWorkerId`), and in `DiffTab` derive the header branch and diff files from that worker (`worker.branch`, per-worker mock diff entries).
2. Or stop presenting Diff as a per-worker action: remove the hero button from `WorkerDetail` and open the Diff tab only from the workbench/top bar, relabeling it as the lead's reviewed-branch stand-in.

Regression test: select a non-Ana worker (e.g. Theo), click Diff, assert the header shows `wt/agent-theo` (direction 1) — or assert the button no longer exists (direction 2). Must fail on today's code.

### H2. The fixer target is not tracked — "Apply mock fixer result" is offered on (and accepts) *any* open finding, writing a false review record

- `mnemo2/src/lib/store.ts:287-291` — `spawnFixer` sets `worker.status = "fixing"` and briefs the fixer on one finding (`store.ts:316` "Fixer brief: ${finding.title}") but never records which finding.
- `mnemo2/src/lib/store.ts:330` — `completeFixer` guards only `worker.status === "fixing"` and `finding.status === "open"`, not that `findingId` is the fixer's target.
- `mnemo2/src/components/FindingsPanel.svelte:45` — the `{:else if worker.status === "fixing"}` branch renders the "Apply mock fixer result" button for **every** finding that is not fixed/wontfix.

Reachable in the shipped default demo: Ana has two findings open at rest (`ana-copy`, `ana-merge` — proven by the green `store.test.ts:47-51`, where both guards require `status === "open"`, and `LeadChat.test.ts:15` "Ana has 2 open findings"); Milo additionally ships in `fixing` status with an open finding (`mock.ts:196-213`). Spawn a fixer on one of Ana's findings → **both** findings render "Apply mock fixer result" → completing the non-target marks it fixed with `fixer-<id>` evidence and appends a "Fixer delta verified · Ana" lead entry (`store.ts:363-376`) whose body is the wrong finding's title, while the actual target stays open and its "Fixer applying review delta" timeline step is silently marked done (`store.ts:350-353`). Merge gating itself stays correct (the still-open target blocks `canMerge`), but the lead chat now carries a false review record.

Fix: add `fixerFindingId?: string` to the `Worker` type (`mnemo2/src/lib/types.ts`); set it in `spawnFixer`; require `findingId === worker.fixerFindingId` in the `completeFixer` guard (`store.ts:330`); clear it on completion and in `stopWorker`/`replyWorker`; render the apply button in `FindingsPanel.svelte:45` only when `worker.fixerFindingId === finding.id` (other open findings show no action or a muted "fixer running" hint).

Regression test (store.test.ts): with both findings open, `spawnFixer("ana", "ana-copy")` then `completeFixer("ana", "ana-merge")` must leave state unchanged; `completeFixer("ana", "ana-copy")` must resolve only `ana-copy`. Fails on today's code.
## Low findings

### L1. Sheet dialog: `aria-modal="true"` with no focus management

- `mnemo2/src/components/Sheet.svelte:29` — `role="dialog" aria-modal="true"`, but nothing moves focus into the dialog on open, the background remains keyboard-tabbable (verified: no `.focus()`, `inert`, or focus-trap anywhere in `mnemo2/src`), and focus is not restored to the invoking button on close. Keyboard/AT users can tab into and operate invisible background controls (e.g. type into the lead composer) behind the "modal".
- Fix: on open, focus the close button (or a `tabindex="-1"` container); set `inert` on `.workspace-shell` (`App.svelte:19`) while a sheet is open; restore focus to the invoker on close.

### L2. Visual-only active state on tab/filter/nav buttons

- `mnemo2/src/components/WorkerDetail.svelte:74-76` — Review/Thread/Timeline detail-tabs convey the active tab only via `class:active`; no `aria-pressed`/`aria-selected`/`aria-current`.
- `mnemo2/src/components/ControlRoom.svelte:59-62` — board filter buttons (All/Needs you/Active/Done): same gap.
- `mnemo2/src/components/TopBar.svelte:32` — Lead nav button: same gap (Team at least exposes `aria-expanded`).
- Fix: add `aria-pressed={tab === "review"}` etc. to all three sets, or promote the worker detail tabs to a proper `role="tablist"`/`role="tab"`/`role="tabpanel"` with `aria-selected`.

### L3. Team nav exposes `aria-expanded` on the Mission Control route, where it cannot expand anything

- `mnemo2/src/components/TopBar.svelte:33` — `aria-expanded={$app.teamPeekOpen}`; on route "team" the handler (`TopBar.svelte:11-14`) calls `openTeam()`, and `TeamPeek` only renders on route "lead" (`App.svelte:31`). The control announces an expandable state it cannot deliver in that route.
- Fix: `aria-expanded={$app.route === "lead" ? $app.teamPeekOpen : undefined}`.

### L4. The two worker-slots inputs disagree on cleared input

- `mnemo2/src/components/TopBar.svelte:17` — `Number(value)`: clearing the field yields `""` → `0` → clamped to **1** (`store.ts:469-478`: `Number.isFinite(0)` passes, `Math.max(1, Math.floor(0))`).
- `mnemo2/src/components/ControlRoom.svelte:36` — `valueAsNumber`: clearing yields `NaN` → guarded no-op (`store.ts:470`).
- Same setting, two inputs, different behavior on the same gesture.
- Fix: use `valueAsNumber` + the existing finite guard in both handlers (`TopBar.svelte:16-18`).

### L5. Shell height `100vh` sits behind mobile dynamic toolbars

- `mnemo2/src/App.svelte:37` — `.workspace-shell { height: 100vh }` with `body { overflow: hidden }` (`app.css:31`). Exact at the 390×844 DevTools preset, but in a real mobile browser (collapsible toolbar) `100vh` exceeds the visible viewport, pushing the composer dock bottom behind the toolbar. Harmless in the eventual Tauri webview (no browser chrome), but the prototype's dev-server target is a plain browser.
- Fix: `.workspace-shell { height: 100vh; height: 100dvh; }` (fallback preserved); optionally the same for `StartScreen.svelte:69,74`.
## Verified good (checked, no finding)

- **Mock-only boundary (risk focus 4):** no Tauri `invoke`, no engine imports, no fs/node/git/child_process, no iframe, no fetch/XHR/WebSocket, no `window.open` anywhere in `mnemo2/src` (searched). Markdown links are intercepted and routed to the in-app mock browser (`Markdown.svelte:34-39` → `actions.openBrowser`); `navigator.clipboard` is used only for the code-copy button and guarded.
- **Security:** lead Markdown renders through markdown-it + highlight.js then DOMPurify (`markdown.ts:1-3`); user messages render as plain text interpolation (`LeadChat.svelte:36`), never `{@html}`.
- **Merge gating (risk focus 1):** `canMerge` (`review.ts:55-81`) requires in_review status, zero open findings, trimmed justification on every wontfix, and all checks passed; the merge button disables on `!eligibility.ok` (`WorkerDetail.svelte:103`) and `mergeWorker` re-guards server-side (`store.ts:415-416`). Tiny-fix policy (`review.ts:27-52`) is coherent and unit-tested. End-to-end flow tested (`ControlRoom.test.ts:29-44`, `store.test.ts:37-74`).
- **Responsive (risk focus 2):** the narrow workbench fix is coherent — `.lead-column` is `position: relative` (`App.svelte:39`), so `position:absolute; inset:0` (`Workbench.svelte:96`) covers exactly the lead column below the 64px topbar; `width: 100% !important` overrides the inline width; the resizer hides and a back button appears ≤980px; desktop >980px is untouched. Layering is correct: workbench z-30 < peek z-35 < mobile nav z-40 < sheet z-60 < toast z-80 — toasts stay visible over the full-screen workbench and the peek footer clears the fixed mobile nav (`TeamPeek.svelte:48` `margin-bottom: 58px`). Mission Control summary wraps at ≤760px (`flex-wrap` + full-basis merge note, `ControlRoom.svelte:127-128`) and board filters scroll (`overflow-x: auto`, line 131) — no horizontal overflow found at 390px.
- **Accessibility (risk focus 3):** the workbench resizer is keyboard-operable (ArrowLeft/Right ±24px with `preventDefault`, `aria-label`, title — `Workbench.svelte:32-54`); sheets close via Escape/backdrop/named close button; RunStrip exposes `aria-expanded`; worker rows, merge, fixer, wontfix, send, and settings controls all carry names/states (remaining gaps are L1-L3 above).
- **State robustness:** `setWorkerSlots` NaN guard (`store.ts:470`); toast timer effect hygiene with cleanup (`Toast.svelte:7-13`); worker/tab resets in `$effect` read only the ids they depend on (no loops); keyed `{#each}` throughout; `resetDemo` rebuilds via `createInitialState`.
- **Documentation sync:** README.md:178-181 commands (`dev:mnemo2`, `check:mnemo2`, `test:mnemo2`, `build:mnemo2`) match `package.json:14-17` exactly; `mnemo2/README.md` run/test instructions and the mock-only scope description are accurate. Module doc comments exist on the policy functions (`review.ts:26,49,54`).
- **Multi-platform neutrality:** nothing Windows-specific in `mnemo2` (no `cfg(windows)`, no Win paths/shell syntax); mock paths are repo-relative forward slashes; the localhost:5180 URL check is platform-neutral.
- **Tests (risk focus 6):** 6 files / 22 tests genuinely exercise the changed behavior — merge gating blocks, fixer flow, wontfix justification guard, stop/conflict routing, tab switching, address normalization, markdown safety, activity expansion. Fix H1/H2 with the regression tests specified above (each red on today's code).
- **File-tools-first / warning-free:** the diff shows only file-tool-authored changes (no shell mutation); reported `cargo test --workspace` exit 0 and Svelte build with 0 errors/warnings — consistent with the sources read (no `#[allow]`, no dead code spotted).

## Bookkeeping accuracy (per contract — one line, no line review)

`.coding/**` changes match what shipped: plan 91755fa7 shows 6/6 steps complete consistent with the delivered workspace; plans 33a7341e/44e57565 and the three new knowledge records (finish-ticks bug, CDP-port rule, complete-frame spec) accurately describe shipped behavior; backlog.jsonl deletions are completed-item cleanup.

## Evidence base

Read in full or in verified slices: `mnemo2/src/App.svelte`, all 14 components, `lib/store.ts` (complete), `lib/mock.ts` (Ana/Milo/Theo/June workers, diff content), `lib/review.ts`, `lib/markdown.ts`, all 6 test files, `index.html`, `favicon.svg`, `package.json` (root + workspace), README.md, mnemo2/README.md, and the `.coding/**` diffs. Branch history walked (`git log`: bf44b0e, 9e882ef); working tree is dirty, so the uncommitted diff is real. No browser instances or smoke scripts were launched, per the user's constraint.