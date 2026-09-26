## Verdict: PASS

Review of ALL uncommitted changes on this working tree (branch `wt/*`) against plan 864ad68e — "Dashboard: live-update while the tab is open (no close/reopen)" (kind=implementation, 5/5 steps). No defects found; every claim in the plan and spawn brief was verified against the working tree. Details below, including the evidence for the judgment calls.

**Scope reviewed** (`git_read diff` + `status`, full diff read): `frontend/src/components/views/DashboardView.tsx`, `frontend/src/components/views/DashboardView.test.tsx`, `docs/FEATURES.md`, `README.md` (modified); `.coding/plans/864ad68e.md` (untracked plan file), plus the out-of-scope leftovers `.coding/plans/58f47a0b.md` (modified) and `.coding/knowledge/bug/58f47a0b.md` (untracked) judged lightly for consistency only.

---

## 1. Correctness — the effect delivers the goal

**Mount semantics — the load-bearing premise, CONFIRMED independently (not hidden-but-mounted).**
- `frontend/src/App.tsx:842-851`: `{rightPanelVisible && (<>… <RightPanel /> </>)}` — closing the panel (store `rightPanelVisible` → false) **unmounts** RightPanel via React conditional rendering, so DashboardView unmounts and the effect cleanup runs. Not a CSS hide.
- `frontend/src/components/layout/RightPanel.tsx:130-136`: only the active tab's `Body` is rendered (`const Body = view.component; return <Body />;`); `shownTab === undefined` renders a placeholder div instead. Switching tabs therefore **unmounts** the Dashboard.
- Consequence: the mount-scoped interval is naturally bounded to "this tab is open" — the plan's step-1 caveat resolved in favor of the mount-scoped-only fix, and the in-code claim at `DashboardView.tsx:117-120` is accurate. The incomplete-fix scenario from the brief (poll ticking behind a closed panel) does not occur.

**Hazard-by-hazard check of the effect (`DashboardView.tsx:121-145`):**
- `cancelled` vs in-flight promise: cleanup sets `cancelled = true` and removes every caller of `tick` (interval + both listeners). An in-flight `refresh` that completes after unmount only calls setState on an unmounted instance — a harmless no-op in React 18 (no warning, no leak). StrictMode's setup→cleanup→setup double-invoke is handled: cleanup clears the first interval, and a shared-`inFlight` remount tick self-corrects within one interval.
- `inFlight` wedge: reset in the `finally` (line 130) on every path, including `refresh` throwing — which it can't internally (its own try/catch, lines 106-110) — so it cannot wedge true. The guard makes the simultaneous focus + visibilitychange double-fire a no-op for the second (the first sets `inFlight.current = true` synchronously before awaiting).
- visibilitychange on the hidden edge: `onFocus` → `tick` → `if (… document.hidden) return` (line 125) — becoming hidden is a no-op, becoming visible refreshes immediately. The tick's own guard is the correct single choke point; no separate handler state needed.
- Double-refresh on mount: none. The immediate `void tick()` (line 133) plus `setInterval` first firing at t=2s cannot overlap; the in-flight guard would dedupe even if they did.
- Stale closures: `refresh` is `useCallback(…, [])` (`DashboardView.tsx:98-111`) — stable identity, closing over only state setters and module functions (no reactive state reads). Omitting it from the mount-once deps is genuinely safe.
- Pre-existing behavior untouched: `loading` starts true and is only ever set false (line 109) — polls never flash the "Loading savings…" gate; error/empty/body gates (`DashboardView.tsx:147-156`) and all markup are unchanged (diff touches only the import line, `POLL_MS` + comment, and the effect). A transient poll error now self-heals on the next tick — an improvement over the mount-once version, not a regression.

## 2. Bugs / regressions / test quality

- **Pre-existing assertions not weakened:** the test diff is pure additions (30 insertions, 0 deletions) — all 8 pre-existing renderToStaticMarkup fixture tests remain byte-identical; 2 new `it`s bring the file to 10, matching the reported run.
- **Vacuous-pass check:** every asserted substring was located in `DashboardView.tsx` — each matches the intended wiring and none appears in an unrelated context (comments included): `const POLL_MS = 2000;` (l.28, unique), `const interval = setInterval(() => void tick(), POLL_MS);` (l.134, unique), `document.hidden` (l.125, the guard, unique), `inFlight.current` (ll.125/126/130, all inside `tick`), both listener registrations (ll.136-137) and removals (ll.141-142), `clearInterval(interval);` (l.140). The test pins every clause the plan's step 3 enumerated: cadence, interval wiring, hidden skip, in-flight guard, focus/visibility registration, and full teardown.
- **Idiom conformance:** `import viewSource from "./DashboardView.tsx?raw";` (test l.23) matches the house precedent exactly (`InflightBar.test.ts:23` imports `"./InflightBar.tsx?raw"`, rationale comment at ll.15-19; interval pin at l.115) — `?raw` is typed by `vite/client` so it compiles under `tsc`, consistent with the implementer's reported `npx tsc --noEmit` → 0.
- Listener leaks / duplicate registration: impossible — effect deps `[]` (mount-once), listeners added once and removed in the same closure's cleanup; the component re-renders every poll but the effect never re-runs.

## 3. Project constitution

- **Documentation sync — accurate.** `docs/FEATURES.md` (Dashboard bullet, "The view re-reads the ledger every 2 s while its tab is open (and on window focus), so new savings appear without closing and reopening it.") and `README.md:193-194` (same clause) both match the shipped behavior: 2 s `POLL_MS` poll, focus/visibility immediate refresh, unmount on tab switch/close. The docs name only "window focus" of the two immediate triggers — an accurate subset (regaining the app always fires focus), not a factual error. **`docs/CONFIGURATION.md:9` is still correct, not stale:** its Dashboard mention is a terse pointer ("reads that ledger back per project") inside a config-knobs paragraph; it makes no update-cadence claim, and the file's own header (l.3) defers feature detail to FEATURES.md. Plan step 4's rule ("update only if it would otherwise be stale") was applied correctly. No other user-facing surface mentions Dashboard update behavior (module doc comments: `POLL_MS` and the effect both carry accurate comments; no config examples involved).
- **Multi-platform neutrality — clean.** Only standard web-platform APIs are used (`document.hidden`, `visibilitychange`, `focus`, `setInterval`), which behave identically in macOS WebKit and Windows WebView2. No Windows-only API, path, or shell assumption anywhere in the change.
- **File-tools-first — clean.** No shell-mutation artifacts in the diff; changes are targeted edits consistent with `file_edit`/`file_write` authorship; line endings clean in the diff.
- **Code style — conformed.** `POLL_MS` has a doc comment (ll.26-28); the effect carries a substantive rationale comment (ll.113-120); the eslint-disable at l.144 is the house idiom verbatim (`PlanProgress.tsx:126`: `// eslint-disable-next-line react-hooks/exhaustive-deps -- mount-once poll; refresh intentionally omitted`) and its justification is **real, not a silencer** — `refresh`'s identity is stable by construction (`useCallback(…, [])`), so including it could never re-run the effect anyway. The interval idiom mirrors `PlanProgress.tsx:124` (`const interval = setInterval(refresh, 2000);`); the `cancelled` flag mirrors `MemoryDebugView`'s. No TS-equivalent of an `#[allow]` escape was added.

## 4. Security

- **IPC amplification — bounded and read-only.** Each tick calls exactly `getSavingsStats()` + `getProjectStats()` (`DashboardView.tsx:102`, unchanged from the pre-existing mount fetch) — **not** `getPricing` (a small correction to the spawn brief: pricing is read from the agent store, `DashboardView.tsx:92`, and is not part of the poll). Both commands are pre-existing read-only local queries: `src-tauri/src/ipc/agent.rs:960-971` (`store.savings_stats()` — SELECT aggregation over the ledger) and `:940-951` (`get_project_stats`, same read-only shape). The poll adds frequency, not surface: 2 read-only queries per 2 s only while the tab is open **and** the document is visible; the in-flight guard prevents stacking if a query ever outlives the interval; unmount stops everything. No new data is exposed; no unbounded growth (responses replace state, no accumulation).

## 5. Out-of-scope leftovers (judged lightly, per the brief)

`.coding/plans/58f47a0b.md` adds a "## Regression test" section naming `is_indexable_path_accepts_any_file_and_rejects_ignored`, and the untracked `.coding/knowledge/bug/58f47a0b.md` BUG record references the same test — the symbol exists (`src/codegraph/watcher.rs:274-364`), so the bookkeeping is internally consistent and points at a real regression test. No action needed for this plan; both are prior-plan (58f47a0b) finish-time artifacts whose source change is already committed.

## 6. Verification performed

Reviewer toolset note: this reviewer's allow-list carries no shell/execution tool, so the suites were not re-executed here; the implementer's recorded runs are quoted and my static review found nothing that would change them.

- `git_read op=diff` — full uncommitted diff (5 modified files, 73 insertions / 5 deletions) read in full; `op=status` — 2 untracked files reviewed.
- Read directly: `DashboardView.tsx` (all 341 lines — effect, `refresh`, render gates), `DashboardView.test.tsx` (ll.1-45 + full new block via diff), `App.tsx:835-879`, `RightPanel.tsx:90-145`, `InflightBar.test.ts:1-40,95-156`, `docs/CONFIGURATION.md:1-16`, `.coding/plans/864ad68e.md` (all 17 lines), `.coding/knowledge/bug/58f47a0b.md`.
- Graph/ed searches: `PlanProgress` idiom (`PlanProgress.tsx:84,101,124,126` — interval + identical eslint-disable precedent), `get_savings_stats` / `get_project_stats` (read-only Rust commands), `is_indexable_path_accepts_any_file_and_rejects_ignored` (exists), `rightPanelVisible` (App gate + store).
- Implementer's recorded verification (quoted, exit 0): from `frontend/` `npx tsc --noEmit` → 0; `npx vitest run` → 92 files / 1297 tests passed (DashboardView.test.tsx = 10); repo root `cargo test --quiet` → 2718 passed / 0 failed / 5 ignored + 19 passed + 1 passed / 1 ignored. Rust sources are untouched by this diff (only 2 frontend files + 2 docs + `.coding` bookkeeping), so the green cargo run is unaffected by the change.

## Non-findings (judgment calls, documented)

- The immediate mount tick (`void tick()`, `DashboardView.tsx:133`) is itself not asserted by the new test — accepted: the plan's step 3 defines the exact pin list and every enumerated clause is asserted; the mount fetch is the pre-existing behavior this change preserves, and the house source-pinning idiom (InflightBar precedent) does not pin every wiring line either.
- The 2 s poll re-renders the view with fresh object identities even when data is unchanged — inherent to the PlanProgress poll idiom the plan explicitly asked to mirror; DashboardBody is a small static tree with no inputs or animation, so the cost is negligible and consistent with house style.
- Docs say "(and on window focus)" while the code also wires `visibilitychange` — an accurate understatement (any return to the app fires focus), not a discrepancy worth a docs churn.

**Conclusion:** the change does exactly what plan 864ad68e scoped, in the house idiom, with the plan's pin list fully covered, docs synced where they enumerate behavior, no platform or security regressions, and no pre-existing behavior altered. Ready to commit.