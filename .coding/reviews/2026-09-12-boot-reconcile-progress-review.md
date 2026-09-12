## Verdict: FINDINGS (0 high, 4 low)

Implementation review of plan bfe5198e "Boot-window reconcile progress (486955d5 completion)" — all uncommitted changes on wt/agenticcoding: `frontend/src/components/common/BootSplash.tsx`, `frontend/src/components/common/BootSplash.test.tsx`, `frontend/src/App.tsx`, plus `.coding/` bookkeeping (backlog flip to in_flight + the untracked plan file) that rides the commit per the established pattern.

The change is functionally correct: the `BootReconcile` type mirrors App's state exactly (no casts), the SplashProgress usage matches the IndexingOverlay pattern, the boot-window → post-boot-dialog handoff is clean (no double-render, no lifecycle conflict), and the new tests genuinely pin the determinate bar and the App source contract. All four findings are low-severity — two documentation-accuracy issues, one weak test assertion, one backend-scoped residual noted for the record. None block the commit; the doc-comment reword (Finding 1) and the README clause (Finding 2) should land with it.

### Verified correct

1. **Type contract (assignability without casts).** App's state (App.tsx:107-112) is `{ phase: "running"; done: number; total: number } | { phase: "done"; summary: string } | { phase: "failed"; error: string } | null` — structurally identical to `BootReconcile` (BootSplash.tsx:13-16), and the prop is typed `reconcile?: BootReconcile | null` (BootSplash.tsx:47). App.tsx:600 passes it directly; no casts anywhere; `npx tsc --noEmit` clean (reported).

2. **SplashProgress usage matches the IndexingOverlay pattern.** BootSplash.tsx:60-72 uses the identical guard shape as IndexingOverlay.tsx:197-208: `right = total > 0 ? \`${done}/${total}\` : "…"`, `pct = total > 0 ? Math.round((done/total)*100) : null`. SplashCard.tsx:145 renders `width: ${pct === null ? 30 : pct}%`, so the test's `width:38%` pin is exact (3/8 → Math.round(37.5) = 38) and `pct = null` yields the 30% indeterminate stub. Division by zero is guarded (`total > 0`) in both the count text and the pct.

3. **Default-null behavior.** `reconcile = null` default (BootSplash.tsx:45): undefined → `reconcile?.phase === "running"` is false → else branch → `reconcile === null ? "Starting…"` (BootSplash.tsx:78-79). Undefined can never fall through to the done/failed branches. Pinned by the pre-existing first test (`<BootSplash />` → "Starting…").

4. **No double-render / lifecycle conflict during the boot window.** The early return (App.tsx:599-601) precedes the reconcile dialog (App.tsx:641-683, inside the main tree return at 637): during the boot window only BootSplash renders; when the checks resolve, BootSplash unmounts and the dialog mounts from the same state — a seamless handoff (running bar → running bar; done hint → done summary + Dismiss). `useBrowserOverlay(reconcile !== null)` (App.tsx:118) is pre-existing and untouched by this diff. The done/failed splash texts ("Memory index ready" / "Memory index check failed") are transient hints that don't duplicate the dialog's role — the dialog still carries the summary/error until dismissed, exactly as the doc comment claims.

5. **Boot-window overlap — conclusion sound (stated mechanism wrong; see Finding 1).** The main window is created before `build_brain` runs in the setup hook (main.rs:288; `app.get_window("main")` at main.rs:316), so the webview loads and React mounts while build_brain runs on the main thread; the startup checks cannot resolve until IpcState is managed after build_brain returns (main.rs:447/573/659). The reconcile is spawned inside build_brain (main.rs:1267) — before it returns — so the boot window always covers the reconcile's start. Events emitted before the listener attaches are lost, but progress events stream one per completed source (main.rs:1367-1373), so a mounted listener catches up on the next event, and a missed `started` self-heals because App's `progress` handler accepts `prev === null` (App.tsx:133-137). A reconcile that outlives the boot window hands off to the post-boot dialog mid-run; one that finishes before React mounts leaves the state null → "Starting…", which is correct (nothing slow left to show).

6. **Tests pin the behavior.** The determinate case pins `width:38%` (distinct from the 30% indeterminate stub), "3/8", the phase text, and the label; the indeterminate case pins the absence of "0/0"; the done case pins the kept spinner; the failed case pins the hint. The source-contract assertion `return <BootSplash reconcile={reconcile} />;` matches App.tsx:600 exactly (verified by search). Reported: 81 files / 1111 tests passed (up 4 from 1107), tsc clean.

7. **Static-splash parity contract holds.** frontend/index.html:27-38's "pixel-identical handoff" claim is unaffected: reconcile events emitted before React mounts are lost, so BootSplash's FIRST render always has `reconcile = null` — the generic twin the static splash matches. The reconcile bar is a post-mount upgrade the pre-React splash cannot (and need not) show. No index.html change needed.

8. **Call sites.** Only App.tsx:600 and the test file reference BootSplash — no other consumer needs the new prop.

### Findings

**Finding 1 (LOW) — doc comments state a wrong mechanism for the boot-window overlap.**
- frontend/src/components/common/BootSplash.tsx:38-40: "the reconcile runs inside `build_brain`, so the startup checks cannot resolve before it finishes — which is why live listening needs no snapshot."
- frontend/src/App.tsx:597-598: "the boot window always overlaps it because build_brain blocks the setup hook until it finishes."
- Reality: the reconcile is spawned to the background (`tauri::async_runtime::spawn`, src-tauri/src/main.rs:1267), and main.rs's own comment (main.rs:1253-1255) says "Runs in the background so startup never waits on it." IpcState is managed after build_brain returns (main.rs:447/573/659), so the startup checks resolve as soon as build_brain finishes — typically WHILE a slow reconcile still runs (that is exactly when the post-boot dialog's running-bar path, App.tsx:652, is live).
- The design conclusion still holds, but for a different reason: the reconcile is spawned before build_brain returns, and the checks cannot resolve before build_brain returns, so the boot window always covers the reconcile's start; streaming progress events catch a late-attaching listener up; a reconcile that outlives the boot window hands off to the post-boot dialog.
- Why it matters: a future maintainer believing "the checks cannot resolve before the reconcile finishes" could treat the dialog's running path as unreachable dead code or mis-reason about the handoff. Fix: reword both comments to the accurate mechanism (spawned-inside-build_brain + checks-gated-on-build_brain-return), e.g. "the reconcile is spawned inside build_brain before it returns, and the startup checks cannot resolve before build_brain returns — so the boot window always covers the reconcile's start; a reconcile that outlives it hands off to the post-boot dialog."

**Finding 2 (LOW) — README not updated for the reconcile-in-boot layer.**
- README.md:57 describes the reconcile feedback as "a branded splash wait dialog … and a live progress bar + counter", and README.md:66 enumerates the open/create feedback layers (static pre-React splash, "a matching BootSplash covers the pre-index boot phases", phase-aware picker buttons, indexing overlay) — neither mentions that the BootSplash itself now upgrades to "Building memory index" + the reconcile bar during the boot window. Per the project's documentation-sync review expectation, the shipped layering should be reflected; one clause in the README.md:66 sentence (or the line-57 description) suffices.

**Finding 3 (LOW) — weak assertion in the indeterminate test.**
- frontend/src/components/common/BootSplash.test.tsx:41: `expect(markup).toContain("…")` is trivially satisfied — the SplashProgress label "Reconciling memories…" (BootSplash.tsx:61) already contains "…", so the assertion passes even if the right-side indeterminate counter regressed to empty. The meaningful pin is `not.toContain("0/0")` (BootSplash.test.tsx:42), which is present. Suggest asserting the mono counter span's content (e.g. `font-mono">…<`) or dropping the redundant assertion.

**Finding 4 (LOW, residual — backend-scoped, not a defect of this diff).**
- The reconcile's pre-event window shows no phase text: the authored-row migration (src-tauri/src/main.rs:1322-1340) and the staleness pre-check (main.rs:1345-1359) run before the first `started` event (main.rs:1363), so during that sub-phase the BootSplash stays on the generic "Starting…". On a large drifted corpus this could exceed ~1s — a residual sliver of the backlog's "every open/create phase longer than ~1s shows a progress bar with phase text" acceptance. Covering it needs an earlier backend event (a Rust change), out of this frontend-only plan's scope, and the pre-check is documented as cheap (main.rs:1248). Recorded for the backlog completion note; no action required in this diff.

### Constitution checks

- **Multi-platform neutrality:** PASS — frontend-only change; no platform APIs, paths, or shell syntax anywhere in the diff.
- **File-tools-first:** PASS — the diff consists of clean, targeted edits consistent with `file_edit`/`file_write`; no shell-mutation signatures.
- **Documentation sync:** BootSplash's doc comment was updated (but see Finding 1 for the inaccurate mechanism sentence and Finding 2 for the README gap). frontend/index.html's static-splash parity comment is unaffected and its contract still holds (Verified correct #7).
- **Tests:** reported 81 files / 1111 tests passed (up 4) and `npx tsc --noEmit` clean; the four new cases and the updated source-contract assertion were reviewed statically and match the implementation. No Rust code touched, so `cargo test` is unaffected.
- **Bookkeeping:** `.coding/backlog.jsonl` (486955d5 → in_flight with the plan pointer) and the untracked `.coding/plans/bfe5198e.md` ride the commit per the established pattern.

### Summary for the main agent

Fix Findings 1-3 (comment reword in BootSplash.tsx:38-40 + App.tsx:597-598; README clause at README.md:57 or :66; optionally tighten BootSplash.test.tsx:41), re-run the frontend tests, and commit. Finding 4 is a recorded residual for the backlog item's completion note — no code change expected in this plan.
