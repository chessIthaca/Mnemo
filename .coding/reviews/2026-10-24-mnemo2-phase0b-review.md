## Verdict: FINDINGS (0 high, 2 low)

Two LOW findings: (F1) an acceptance-critical UI behavior — tab reset on worker switch — has no automated regression test; (F2) `text-overflow: ellipsis` is applied to the ModelBadge flex container where it is a no-op, so over-long model names would hard-clip without an ellipsis. No high-severity findings; the workflow derivation, merge gates, mock-only boundary, minimalism, and documentation sync all check out.

### Scope of review

The working tree is not clean — the uncommitted diff on `wt/mnemo` (base: branch tip `1ec7958`, stamped as this round's review base in `.coding/plans/790256c6.md`) is the Phase 0b delta. I read the current state of every changed/new file end-to-end rather than relying on the archived diff: `workflow.ts`, `workflow.test.ts`, `types.ts`, `mock.ts`, `store.ts`, `store.test.ts`, `review.ts` (unchanged, read to verify the gate), `LeadChat.svelte` + test, `StartScreen.svelte`, `SpawnCard.svelte`, `WorkerRow.svelte`, `ControlRoom.svelte` + test, `WorkerDetail.svelte`, `ModelBadge.svelte`, `TaskWorkflow.svelte`, `Composer.svelte`, `TopBar.svelte`, `BrowserTab.svelte`, `Markdown.svelte`, `.coding/design/mnemo2.md`, `README.md` (Phase 0b section), `mnemo2/README.md`, `.coding/plans/790256c6.md`, `.coding/plans/91755fa7.md`, and the new decision knowledge record. `RunStrip.svelte` is deleted; searches for `RunStrip`, `leadTimeline|activityExpanded|toggleActivity`, `Run 04`, and `__TAURI__|invoke(|mnemo::|@tauri-apps` confirm zero references in `mnemo2/src` (the only `RunStrip` hits are historical plans/reviews, and `Run 04` appears solely in LeadChat's negative assertion). I did not launch browsers, smoke scripts, or extra instances, per the task's explicit constraint; responsive claims rest on the dispatcher's reported 390/594/1440 px live checks plus my static review of the overflow/min-width CSS.

### F1 (LOW): worker-switch tab reset is untested

`WorkerDetail.svelte:28-34` resets `tab` to `"thread"` and clears `reply` when `worker.id` changes — the acceptance criterion "selection changes reset correctly; updates to the same worker do not reset the active tab." Coverage today:

- Thread default on first render: tested (`ControlRoom.test.ts:29-34`).
- Same-worker updates don't reset: implicitly proven by the merge-drive test (`ControlRoom.test.ts:36-52`), which clicks Review-tab buttons (`Fix tiny`, `Spawn fixer`, `Apply mock fixer result`) across successive same-worker store mutations — if the effect fired on any update, those buttons would vanish and the test would fail.
- Switch-to-another-worker resets the tab: **no test**. A future refactor that re-keys the effect (e.g. on the whole `worker` object, or resetting inside `replyWorker`) would silently reset the tab on every update, and nothing would fail.

**Fix (suggested ~5 lines in `ControlRoom.test.ts`):** open Review on the default worker, click `Open Theo worker thread`, then assert `screen.getByRole("button", { name: /^Thread/ }).getAttribute("aria-pressed")` is `"true"` — mirroring the pattern already used at line 31. Optionally also assert the Review button is not pressed, pinning the reset direction.

### F2 (LOW): `text-overflow: ellipsis` never renders on the ModelBadge

`ModelBadge.svelte:19` sets `.model-badge { display: inline-flex; … }`, and the model text is a direct anonymous flex item (line 16). Per CSS, `text-overflow` applies to block containers only — on a flex container it is a no-op. The ellipsis declared at call sites is therefore dead CSS:

- `WorkerDetail.svelte:144` — `.worker-title :global(.model-badge) { max-width: 180px; overflow: hidden; text-overflow: ellipsis; }`
- `WorkerRow.svelte:51` — `max-width: 130px; … overflow: hidden; text-overflow: ellipsis`
- `StartScreen.svelte:74` — `max-width: 145px; overflow: hidden; text-overflow: ellipsis`
- `SpawnCard.svelte:43` — `max-width: 130px` (no `overflow: hidden` at all)

Today's mock names fit the caps (the longest, `kimi-k3.1-pro-highspeed`, is borderline at 130px), so nothing visibly breaks — but Phase 1 binds real endpoint model ids, which are frequently longer. When a name exceeds the cap it clips mid-glyph with no "…" indicator (and in SpawnCard's chip the nowrap text can paint past the badge box). The `aria-label`/`title` on the badge still carry the full name, so this is cosmetic only.

**Fix (in `ModelBadge.svelte`, fixes every call site at once):** wrap the text — `<span class="dot" aria-hidden="true"></span><span class="model-name">{model}</span>` — and add `.model-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }`. The outer per-site `max-width` + `overflow: hidden` constraints then produce a real ellipsis. Add `overflow: hidden` to the `SpawnCard.svelte:43` override for consistency.

Reviewed-state: 1ec79580b8fb7c1bcea890560e64430a93e44613
### Verified against the acceptance criteria (all pass)

- **Workflow derivation (risk 1).** `deriveTaskWorkflow` (`workflow.ts:24-118`) is a pure function of one worker — no global index, no module-level mutable state (only the immutable `STEP_ORDER`/`STEP_LABELS` shape constants). The switch over `worker.status` is exhaustive against the 9-value `WorkerStatus` union in `types.ts` (svelte-check 0 errors would otherwise fail the build). Transitions are correct and sensible: `fixing` re-derives verify/review from the worker's own checks and fixer finding; `conflict` marks Merge blocked with route-back detail; `in_review` gates Merge on this task's findings+checks only; `failed`/`cancelled`/`merged` render distinctly. `TaskWorkflow.svelte` guards the `findIndex` -1 case with `Math.max(0, …)` and a `?? steps[0]` fallback. Tests pin parallel independence (ana/kai/milo/theo/june in five different states), step order for all 10 workers, merge gating, and terminal states.
- **Worker detail/reset (risk 2).** Thread is the default tab (`WorkerDetail.svelte:20`); the id-keyed effect (lines 28-34) resets tab+reply only on switch (same-worker updates covered implicitly by the ControlRoom merge-drive test — see F1 for the untested leg). Review/merge gates intact: the merge button (line ~101) is `disabled={!eligibility.ok}` from `canMerge` (`review.ts:55`, unchanged) and `store.mergeWorker` re-guards with a toast; `store.test.ts` proves the open-finding block, wontfix justification, targeted fixer finding, and merge record. Conflicted route-back + stop (`store.test.ts:75-83`) still works.
- **Model identity (risk 3).** No stale labels: searches show no `ATLAS CORE`/`Spark Mini` leftovers; LeadChat asserts the fictional label is gone and `claude-opus-5-5` is visible. Badges appear in lead header (`LeadChat.svelte:22`), composer meta (`Composer.svelte:42`), worker rows (`WorkerRow.svelte:30` — and rows keep the badge in compact/narrow since only the cost line is hidden at ≤1100px), delegation chips (`SpawnCard.svelte:22`), worker detail heading, start-screen project cards (`StartScreen.svelte:36`), and settings (TopBar sheet lead model + team default; BrowserTab settings team default). Accessible names are sane: `Lead/Worker model: X` badges, `Open X worker thread` rows, `Merge reviewed branch`, `Task workflow: …` list label, `aria-pressed`/`aria-current` states.
- **Minimalism (risk 4).** `RunStrip.svelte` deleted with zero source references; the lead chat is a compact header + entries + composer; StartScreen is a project-first picker with no marketing hero or Mission Control preview; no explanatory paragraphs reintroduced (the remaining explanatory copy in BrowserTab/TopBar settings is deliberately the sanctioned "what is mocked" labeling).
- **Responsive/overflow (risk 5).** `.lead-workspace`/`.lead-content`/`.lead-scroll` all carry `min-width: 0` (LeadChat.svelte:70-72) so the lead column can shrink; `Markdown.svelte:52-61` gives the container `min-width: 0; max-width: 100%; overflow-x: auto; overflow-wrap: anywhere` so tables/code scroll inside the column instead of pushing the layout. Breakpoints at 1100/980/760/560px progressively hide tertiary chrome. Static review found no new overflow risks; live 390/594/1440 px checks were the dispatcher's.
- **Runes/a11y/tests/docs/platform/policy (risk 6).** Svelte 5 runes used correctly throughout (`$state`/`$derived`/`$derived.by`/`$effect`/`$props`; the WorkerDetail effect intentionally reads a non-reactive `lastWorkerId` so its own writes don't re-trigger; Markdown's `renderVersion` guard prevents stale async HTML). Cross-platform clean — pure web frontend, no Windows-only APIs; WebView2 mentions are Phase 4 design prose inside the sanctioned exception. No shell-based file mutation artifacts; no `#[allow(...)]`/warnings path touched (frontend only; `cargo test --workspace` green is the Rust proof). Docs sync verified below. Dispatcher-reported gates: mnemo2 32 tests + svelte-check 0/0 + build, frontend 1,352 + build, cargo 3,112 — not re-run by me (read-only; no constraint violated by trusting these).
- **Mock-only boundary (risk 7).** No Tauri `invoke`, no `@tauri-apps`, no `mnemo::`/engine imports, no git/filesystem access anywhere in `mnemo2/src`; all state transitions are in-memory store writes (`cargo test --workspace` strings in mock.ts are display labels, not commands). BrowserTab is a labelled stand-in.

### Documentation sync — PASS

`README.md:174-184` describes Phase 0b accurately (conversation-first, per-task workflow, visible models, mock-only). `mnemo2/README.md` tour/"What is mocked"/policy/Next-phases sections match shipped behavior exactly (Thread-first, model badges, merge gate, ten workers, no global run). `.coding/design/mnemo2.md` v0.2 is coherent end-to-end: §1.2 workflow contract, §6 Mission Control (Thread primary, Review secondary), §11 Phases 1-4 each carry concrete deliverables and acceptance criteria, §12 references real source paths.

### Bookkeeping accuracy (one line each)

- `.coding/plans/790256c6.md` — accurate: 5/5 steps match the shipped artifacts step-by-step (RunStrip deletion, store API removals, new primitives, test design, gates), review base `1ec79580` equals the branch tip.
- `.coding/plans/91755fa7.md` (+7) — accurate: the three review-round stamps at `bf44b0e` match the committed round-1/2/3 reports; cost report is harness-generated.
- Decision record `2027-01-11-mnemo2-conversation-first-ui-per-task-workflows.md` — accurate: its four directives are all implemented in this delta.

### Non-blocking observation (not counted in the verdict)

`TopBar.svelte:64-65` carry anomalous leading indentation (10 spaces vs the file's 2-space convention). Cosmetic only; tidy up if those lines are touched for any reason.