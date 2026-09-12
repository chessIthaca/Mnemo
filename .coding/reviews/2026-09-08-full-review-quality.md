## Verdict: FINDINGS (0 high, 4 low)

Full-codebase code-quality & maintainability review of Mnemo at HEAD afd527d (wt/agenticcoding) plus the uncommitted spawn-cancellation / UI-event-delivery delta, 2026-09-08. One-line summary: the tree is in the best shape of any round so far — every fresh feature (backlog position param + soft-delete, parallel run-all worktrees, boundary-token escaping, bug-fixing model slot, fill-rate compact gate, finish-gate re-index) meets or exceeds the project's documentation and test bars — and all four findings are LOWs inside the never-reviewed uncommitted delta: a residual ordering window in the new `has_ever_started` latch, a CWD-dependent diagnostic sink, an untested listener-registration-failure path, and two cosmetic nits.

## Scope & method

- **Target**: working tree at HEAD afd527d on wt/agenticcoding, including the uncommitted delta — `src/runtime/channels.rs` (has_ever_started latch), `src-tauri/src/ipc/events.rs` (cleanup filter + cleanup_tests + emit diagnostics + WEBVIEW_PROBE_JS + diag_line), `src-tauri/src/ipc/agent.rs` (ui_diag), `src-tauri/src/main.rs` (registration), `src-tauri/src/console.rs` + `src/runtime/mod.rs` (tests), `frontend/src/hooks/useAgentEvents.ts` (buffering/attach-detach), `frontend/src/lib/tauri.ts` (uiDiag), `frontend/src/hooks/useAgentEvents.dispatch.test.ts`.
- **Method**: read all six dedupe reports first (the 2027-01-06 round + this round's model-pipeline/performance/memory siblings); full read of the uncommitted diff; targeted reads of every fresh-commit surface since 9a0d5b9 (backlog store/tool/IPC/frontend, `src/project/worktrees.rs`, `src/codegraph/mod.rs` reindex, `src/provider/boundary.rs`, settings bug-fixing slot, run_all.rs compact gate + prompt steering + parallel section, BacklogView detail bar); systematic sweeps (`#[allow(`, TODO/FIXME, `let _ =` / `.ok()`, temp_dir/CWD patterns, vitest registration); doc-comment verification of every new pub item; README/PLAN sync check per fresh feature.
- **Dedupe honored**: nothing from the 2027-01-06 round (all fixed + verified) or the three sibling reports is re-reported (memory report owns the three stale-doc findings; performance owns codegraph.db seeding / reindex escalation scan / DISPATCH_LOCK; model-pipeline owns the 429-fallback viability check).

## Verified solid (spot-checks, no findings)

1. **Backlog position param + soft-delete (534aef8, afd527d)** — `src/backlog.rs`: exemplary module/field docs (the `deleted_at` rationale — git union-merge resurrection — is documented at the field, the module, and README:79); tests cover position insert, deletion stickiness across parse, the 30-day purge age gate, clear-finished soft-delete semantics, and legacy-line parsing. The param is symmetric across store → tool (`src/tool/workflow/backlog.rs`) → IPC (`src-tauri/src/ipc/backlog_cmds.rs`, with a source-contract test at 710-743) → frontend (`BacklogView.tsx` `moveIdToFront`/`moveIdToBack` at 340/353).
2. **Parallel run-all worktrees (48a35ad)** — `src/project/worktrees.rs`: strong module docs (the two-agents-one-tree rationale), real-git tests; the three `let _ =` sites (122, 216, 273) are all non-durable cleanup paths (prune / merge-abort / worktree removal) with the best-effort intent documented.
3. **Finish-gate re-index (3d13884)** — `src/codegraph/mod.rs:525-597`: the flag-bypass rationale is documented in the doc comment (520-524) and tested (`reindex_files_containing_reparses_despite_fresh_meta`, line 746).
4. **Boundary-token escaping (47c36ca)** — `src/provider/boundary.rs:1-40`: module docs state the incident, the marker form, the single-pass identity property, and the collision caveat. (Model-pipeline verified the behavior; the quality axis confirms the docs.)
5. **Bug-fixing model slot (405be6d)** — `src-tauri/src/ipc/settings.rs:480-484` documented; run_all.rs tests cover dispatch landing, prompt steering, and unset-slot inheritance.
6. **Fill-rate compact gate (d92b117)** — `should_between_items_compact` tested at run_all.rs:2696+ including the proxy-ceiling cap, plus a source-contract test for the wiring.
7. **Spawn-cancellation fix (uncommitted)** — three test layers: the latch test (`src/runtime/mod.rs`), the bookkeeping test with the pending child (`src-tauri/src/console.rs:2611-2619`), and the raw-filter `cleanup_tests` (`src-tauri/src/ipc/events.rs:1424+`, four scenarios). The filter's doc comment documents the live incident and the exact semantics.
8. **Emit diagnostics (uncommitted)** — `events.rs:944-1079`: capped (`EMIT_DIAG_LOG_LIMIT`), the doc comment explains tauri's silent-no-op mechanism and why the file sink exists; `.gitignore:50` covers `.coding/logs/`.
9. **Frontend buffering (uncommitted)** — `useAgentEvents.ts:62-140`: bounded buffer (cap 2000, oldest-drop), once-per-episode logging, guarded detach; two new tests cover buffer-then-replay and straight-through delivery; registered in `frontend/vitest.config.ts:45`.
10. **run_all.rs size (6091 lines)** — managed: ~120-line module doc with architecture and invariants, `// ----` section markers (572, 877, 950, 1053, 1325, …), two test modules (tests at 567, extract_tests at 3004), dense rationale comments through `on_main_turn_resolved`. See suggestion 1 for the residual risk.
11. **memory/mod.rs (2211) / tool/memory/mod.rs (2070) / indexer.rs (2571)** — managed: `memory/` is already split into 9 submodules with mod.rs holding the store impl; indexer.rs opens with design invariants (idempotent+incremental, removal detection, pointer-first, graceful degradation); tool/memory/mod.rs documents the full tool roster and the auto-run rationale.
12. **Dead-code/debris sweeps** — zero `#[allow(...)]` in either crate (only doc-comment mentions at backlog_cmds.rs:786, main.rs:917; vendor/tao excluded); zero TODO/FIXME outside test fixtures (safety_rules.rs:579-585); the prior round's `extract_checkpoint_sha` `#[allow(dead_code)]` is gone (now pub(crate) + tested, run_all.rs:2984).
13. **Doc-comment constitution** — every new pub item in the fresh delta and fresh commits carries a doc comment (channels.rs latch field + methods, events.rs diag_line/emit_payload/cleanup filter, agent.rs ui_diag:456-462, tauri.ts uiDiag:1551-1556, backlog position/soft-delete, worktrees, reindex_files_containing, boundary).

## Findings

### LOW-1 — `set_running` latches before flipping `running`, leaving a residual cancellable window
- **Location**: `src/runtime/channels.rs:1048-1055` (`AgentHandle::set_running`); consumer `src-tauri/src/ipc/events.rs:1226+` (`cleanup_inactive_subagents` filter `!h.is_running() && h.has_ever_started()`).
- **Symptom**: `set_running(true)` stores `has_ever_started = true` BEFORE `running = true`. Between the two stores the handle reads as "started + idle" — exactly the state the cleanup filter cancels. A `spawn_agent` whose cleanup pass interleaves in that window cancels an agent whose first turn is just beginning — a nanosecond-scale residual instance of the very race this uncommitted fix eliminates (and on weakly-ordered ARM64, Relaxed store-store reordering can widen it).
- **Why it matters**: the fix's entire purpose is that a just-registered/starting subagent is never cancelled by a later spawn's cleanup; the store order reintroduces a (much smaller) version of the same hole.
- **Fix direction**: swap the stores — `running.store(true)` first, then the latch. Every intermediate state is then spared: (running=false, latch=false) fails `has_ever_started`; (running=true, …) fails `!is_running()`. If airtightness on weakly-ordered targets matters, use Release on the running store (or fold both flags into one atomic state word).

### LOW-2 — `diag_line`'s file sink resolves against the process CWD
- **Location**: `src-tauri/src/ipc/events.rs:1036-1060` (`diag_line`, path `.coding/logs/emit-diag.log`).
- **Symptom**: the sink the doc comment calls "the primary channel for delivery diagnostics" writes to a CWD-relative path. Launched from the project root (dev), it lands in the gitignored `.coding/logs/`; a packaged GUI build's CWD is the exe's directory (Windows, typically non-writable) or `/` (macOS) — so in exactly the environment where stderr also vanishes (the scenario the sink exists for), it silently writes nothing, or somewhere the user will never find.
- **Why it matters**: the diagnostic's target failure mode is the packaged-build event blackout; a CWD-dependent sink is absent precisely there.
- **Fix direction**: anchor the sink CWD-independently the way the watchdog's hang reports already do — `std::env::temp_dir().join("mnemo-hang-reports")` (`src-tauri/src/main.rs:545, 629`) — or the app data dir; alternatively state the dev-only assumption in the doc comment.

### LOW-3 — the listener-registration rejection path has no regression test
- **Location**: `frontend/src/hooks/useAgentEvents.ts:244-266` (`ensureListenerStarted`'s `.catch`).
- **Symptom**: the fresh hardening added tests for buffering-while-detached and replay-on-attach (`useAgentEvents.dispatch.test.ts`), but the third leg — the FATAL console.error + uiDiag when `listen()` itself rejects (denied core:event permission, IPC failure), the one failure that leaves the window with NO listener at all — is untested; a sweep of all frontend test files finds no coverage of it.
- **Why it matters**: this is the same "app works but the UI never updates" symptom class the whole delta exists to eliminate; a regression that swaps the `.catch` back to a fire-and-forget discard would ship silently.
- **Fix direction**: a source-contract test in the established pattern (cf. `src-tauri/src/ipc/backlog_cmds.rs:710-743`, run_all.rs's wiring tests) asserting the registration promise carries a `.catch` that reports via console.error + uiDiag.

### LOW-4 — cosmetic debris in fresh code (two nits)
- **Location**: `src-tauri/src/ipc/events.rs:1478`; `frontend/src/hooks/useAgentEvents.ts:144`.
- **Symptom**: (a) the cleanup_tests assertion message contains a ~14-space run mid-sentence ("it is the              agent the previous spawn_agent call just registered"); (b) `AGENT_EVENT_LABEL` holds the channel name `"agent://event"` — the Rust side's name for the same string is `AGENT_EVENT_CHANNEL`, and the constant exists precisely so the two sides' diagnostic lines can be compared.
- **Why it matters**: trivial, but both are in never-reviewed code, and the naming one works against the cross-side comparison the diagnostic exists for.
- **Fix direction**: collapse the whitespace run; rename to `AGENT_EVENT_CHANNEL` (or add an alias) for cross-side consistency.

## Prioritized improvement suggestions (explicitly requested — nothing broken)

1. **Decompose run_all.rs's two largest resolution functions** — `on_main_turn_resolved` (~794 lines, 4548-5342) and `run_all_dispatch_next` (~537 lines, 3263-3800) — into per-outcome helpers. The file's size is otherwise managed (module docs, section markers, two test modules, dense rationale comments), but these two are the codebase's largest functions and every future run-all change lands in them. The run_turn decomposition from the 2027-01-06 round is the template.
2. **Anchor the diag sink CWD-independently** (LOW-2's fix) — temp_dir or app-data dir, matching the watchdog's hang-report pattern.
3. **Add the registration-rejection source-contract test** (LOW-3's fix) — cheap, established pattern, closes the last untested leg of the event-delivery hardening.
4. **Split indexer.rs's per-source indexers** (plans/reviews/backlog/knowledge) into submodules the way `memory/` already did — its module docs and invariants are already section-shaped; 2571 lines is the largest of the memory family.
5. **Consider a shared diag module** if emit-side diagnostics grow further (`diag_line` + `EMIT_DIAG_LOG_LIMIT` + the probe currently live in events.rs; `ui_diag` in agent.rs) — fine at current size, worth it before a third diagnostic family appears.

## Constitution checks

- **Documentation sync: PASS** — README.md:79 (backlog position param, soft-delete + 30-day purge, detail bar, deferred semantics) and :81 (parallel run-all), PLAN.md:70 (position param), :288 (boundary-token escaping), :126 (auto-compact fill-rate gate) all match the shipped behavior; every fresh feature is documented. (The memory report's three stale-doc findings are excluded per dedupe.)
- **Multi-platform neutrality: PASS** — the fresh delta uses no platform-specific APIs: the diag sink path is a forward-slash relative `Path` (valid on both platforms via std), `create_dir_all` is cross-platform, `WEBVIEW_PROBE_JS` is webview JS, and the frontend changes are platform-neutral. The sanctioned WebView2/game_* exception is untouched.
- **Warning-free build / doc comments: verified by inspection** (read-only reviewer — no cargo run). Zero `#[allow(...)]` in either crate; every new pub item in the fresh delta and fresh commits carries a doc comment; no unused imports or dead code found in the sweeps.
- **Regression tests for recent fixes: PASS with one gap** — the spawn-cancellation fix has three test layers; the buffering fix has two new tests; the boundary/compact/bug-fixing-slot/reindex commits all ship tests. The one gap is LOW-3.

## Bottom line

0 high, 4 low. The committed tree is in the best shape of any round so far — the fresh feature work (backlog position/soft-delete, parallel run-all, boundary escaping, bug-fixing slot, compact gate, finish-gate re-index) meets or exceeds the project's documentation and test bars, and the prior rounds' fixes all verified as landed. All four findings sit in the uncommitted delta: one real (if tiny) residual race window in the new latch, one environment-dependent diagnostic sink, one missing test for the registration-failure path, and two cosmetic nits. All are cheap fixes; none block the delta from landing.
