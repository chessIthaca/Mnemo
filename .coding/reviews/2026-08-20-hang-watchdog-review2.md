# Review 2: Hang watchdog + main-thread blocking audit (plan 7fb6d24c) — follow-up session

Reviewed 2026-08-21 against `git diff HEAD` (12 modified files + untracked
`src-tauri/src/watchdog.rs` and `.coding` bookkeeping). This is the second
review of the plan's uncommitted work: the previous review
(`2026-08-20-hang-watchdog-review.md`) produced 5 findings (F1–F5), of which
F2 (CPU-window math) and F3 (main-thread Rust id) were fixed before this
session, and F1 (console embedder regression), F4 (watchdog_note tests), and
F5 (README/PLAN.md docs) were fixed in this session. This review verifies the
F1/F4/F5 fixes plus the E0658 block fix, and re-checks the whole diff for
correctness, security, and constitution compliance.

Verdict: **the three in-session fixes are correct, the E0658 fix is correct,
and the two earlier fixes are confirmed in the current tree. 1 Low
correctness finding (a pre-existing race shape now reachable on every
startup), 2 nits.** Nothing blocks commit; the Low finding is worth a small
guard.

---

## Findings

### 1. Low (correctness, race) — `install_embedder` can clobber a model the user switches to while the deferred load is in flight

`src-tauri/src/ipc/embeddings.rs:190-195` + `:302-329`: `run_startup_load`'s
success arm unconditionally calls `install_embedder(store, Arc::new(embedder))`
after the background ONNX load completes, and `install_embedder` unconditionally
runs `store.set_embedder(embedder)` (line 327, "Swap the live embedder either
way"). The load runs in the background for 1–30 s after startup (a ~110 MB ONNX
init), so a user who opens Settings and selects a **different** bundled model
in that window has the rewire's embedder silently overwritten by the
startup-model embedder when the load lands. The shared `embedder_status` Arc
then disagrees with the live store: status shows the newly-selected model's
`Ready` (the rewire wrote it after the load's `Ready`), while the store serves
the old model — if the models have different dims (384 vs 768), recall goes to
zero vectors until the next switch/restart.

Mitigations in the codebase: this is the **same race shape as the pre-existing
download path** (`spawn_startup_download` → `install_embedder`, unchanged since
before this plan), so it is consistent with an accepted pattern, and the window
is small (model load during startup + a Settings switch inside it). But the
local-load deferral makes the race reachable on **every** startup with an
installed model, not just first-run. Suggested guard (cheap): before swapping,
check `store.embedder_handle().model_id()` (or the current config's
`bundled_embedding_model`) against the load's `model`; skip the swap when they
differ, since a newer selection wins. If you consider the pre-existing
download-path race a separate accepted trade-off, document the choice in
`install_embedder`'s doc comment. No regression test is demanded here (race,
hard to unit-test deterministically), but a code comment on the guard is
required if fixed.

### 2. Nit (dead code) — `episodes` is `Arc<AtomicU64>` but never shared

`src-tauri/src/watchdog.rs:311` — `let episodes = Arc::new(AtomicU64::new(0));`
is only ever touched by the detector thread itself (line 349). A plain `u64`
would do; the `Arc` wrapper is a vestige (maybe of an earlier design that
shared the counter). Not a warning (`AtomicU64`/`Arc` are used elsewhere in the
module), not a bug — cosmetic cleanup.

### 3. Nit (module docs) — embeddings.rs module doc covers only the download path

`src-tauri/src/ipc/embeddings.rs:13-16` — the module header describes the
first-run *download* startup path (`spawn_startup_download`) but not the new
installed-model *local-load* path (`spawn_startup_load` /
`spawn_startup_load_console`). The new functions carry complete doc comments of
their own, so this is an omission, not stale text. One sentence in the module
header ("an installed model is loaded in the background and swapped in too…")
closes it.

---

## Verified clean (no findings)

### F1 (HIGH, prior review) — console embedder regression: fix correct

- **Shared pipeline extracted correctly.** `run_startup_load` (embeddings.rs:175-208)
  is the single implementation; `spawn_startup_load` (GUI) wraps it with a
  `tauri::async_runtime::spawn` + an `app.emit("embedder://status", s)` closure;
  `spawn_startup_load_console` (console) wraps it with `tokio::spawn` + a no-op
  closure. Runtime choice is right on both sides: the GUI path runs on Tauri's
  async runtime (`tauri::async_runtime::spawn` — required, `app.emit` is a
  Tauri API), the console runs on its own multi-thread tokio runtime
  (main.rs:122-127) where `tokio::spawn` + `spawn_blocking` are correct. No
  cross-runtime spawning, no `tauri::async_runtime` usage from the console.
- **Console hook placement.** console.rs:1367-1380 runs after `BrainOutcome::Ready`
  resolves and before `ConsoleRuntime::start`; it clones the store + status Arc
  and reads `brain.pending_model_load`. No locks are held across the spawn;
  `run_startup_load` awaits `spawn_blocking` (ONNX init) and then
  `install_embedder` (store ops) — neither touches the REPL's locks, so no
  deadlock. Fire-and-forget: `/exit` before the load lands simply drops the
  task at `process::exit` (main.rs:127), matching the GUI's pre-existing
  behavior.
- **Status transitions.** build_brain sets `Checking` in the
  `UseHashThenLocalLoad` arm (main.rs:1085-1087); the load flips `Ready` on
  success / `Failed` on both error arms (embeddings.rs:190-207). `BundledEmbedder::new`
  itself also flips `Ready` (embedder.rs:159) — the outer write is idempotent.
  The re-embed safety block in build_brain is now skipped when
  `pending_load.is_some()` (main.rs:1146-1147), so the interim hash embedder
  can never destroy stored vectors — same guard as the download path.
- **The silent-console-regression is closed.** Both consumers (`console.rs:1375`,
  main.rs:477) read the same `pending_model_load` field; the console is no
  longer stuck on hash with status `Checking` for the session.

### F2 (prior review, fixed before this session) — CPU window math: correct in the tree

`watchdog.rs:305-320` capture the baseline at `start`; `:345-346` sample per
round; the fire arm (`:354-367`) diffs the fire-time sample against
`times_at_last_heartbeat`, which is rolled forward **only when the heartbeat
was observed to advance** (`:394-400`), and divides by `stall_ms` (the stall
window itself, `stall_ms.max(1)` guards division). A 10 s busy stall an hour
into a session now reports ≈100%. The `prev_hb_ms` initial-0 edge (first
heartbeat of ~0 ms never triggers the roll) is fine — the startup sample is the
right baseline for the first window. `saturating_sub` guards FILETIME
arithmetic.

### F3 (prior review, fixed before this session) — main-thread Rust id: correct

`main_thread_rust_id` is captured in `start` (watchdog.rs:254) — the main
thread — not in `detector_loop`, and passed in (watchdog.rs:307). Matches the
`cfg(windows)` OS id capture.

### E0658 (attributes on expressions) — fixed correctly

watchdog.rs:394-400 groups both Windows-only assignments (`times_at_last_heartbeat
= sample_times;` and `prev_hb_ms = last_hb_ms;`) into a single
`#[cfg(windows)] { … }` block — no bare attribute on an expression statement,
so no E0658. Verified there is no other bare `#[cfg(windows)]` on a statement
in the module; all other Windows-only bits are `#[cfg(windows)]` on
fn/let/param positions, which are legal.

### F4 (prior review) — watchdog_note tests: present, meaningful, would fail without the fix

`events.rs` `watchdog_note_tests`:
- `state_changing_events_produce_notes` covers all ten arms (Started, Finished,
  Error retrying, Error final, WorkflowStateChanged, ApprovalRequest,
  UserQuestion, PromptDispatched, SkillStarted, Exited) and asserts the exact
  note text including the `→` in the workflow note. Every `format!` arm is
  exercised; if any arm returned `None`/wrong text, the assert fails. Verified
  the expected strings match the `format!` calls exactly (including the arrow
  character U+2192 at events.rs:59 vs :895).
- `streaming_and_display_events_are_skipped` covers 15 variants (TextDelta,
  ReasoningDelta, ToolCallStart, ToolCallArgDelta, ToolResult, Usage,
  ContextUsage, StepCompleted, SuggestionInjected, ModelChanged,
  MemoryRecalled, Phase, ChildFinished, Compacted) and asserts `None` — this
  is the "would fail without the fix" guard for the `_ => return None` rule.
  All constructors verified compilable: `ToolResult::success` (tool/mod.rs:65),
  `ContextBreakdown: Default` (runtime/channels.rs:45), `FinishReason::Stop`
  (provider/mod.rs:331), `PhaseKind::Streaming` (channels.rs:317), `AgentId =
  u64`, `WorkflowState` Display writes `"Reviewing"` (workflow/mod.rs:51-60).

### F5 (prior review) — docs: synced

README.md adds a feature bullet with the report location
(`.coding/logs/hang-*.txt`, `%TEMP%\mnemo-hang-reports\` pre-project), the
stall threshold, the CPU busy-vs-blocked sample, and the activity ring —
accurate vs the implementation. PLAN.md's decisions table adds the Hang
diagnostics row naming the detector cadence, threshold, report content, and
the cross-platform/`cfg(windows)` split — accurate. The `UseHashThenLocalLoad`
enum doc (client_factory.rs:314-319), `Brain.pending_model_load` doc, and the
`build_brain` timing doc (main.rs:873-916, with the measured 537 ms baseline)
are all consistent.

### Watchdog wiring (all three branches + exit)

- Ready branch starts the watchdog with `backlog_root/.coding/logs`
  (main.rs:401-404) — the project logs dir, created by `write_report` on
  first use; NeedsProject/Err branches use `temp_dir()/mnemo-hang-reports`
  (main.rs:510, 585). All three `IpcState` constructions carry
  `watchdog: Some(...)` (main.rs:430, 548, 621).
- `RunEvent::Exit` stops the watchdog **before** the deliberately-blocking
  (≤10 s) teardown (main.rs:729-735) — the stop-first ordering is correct;
  `stop()` is an idempotent relaxed atomic store, and the detector thread
  checks it every ping (≤500 ms exit latency).
- Forwarder hook: events.rs:252-256 calls `app.state::<IpcState>()` only for
  state-changing events. Can it panic (unmanaged state)? No: the forwarder is
  spawned at main.rs:381 but the only pre-manage event in the channel is the
  main agent's startup `ContextUsage` (spawn.rs:192-201), which `watchdog_note`
  maps to `None` (it is in the skipped list) — so the `app.state` call is
  unreachable before `app.manage` at main.rs:406, and no later event can
  precede manage. `Manager` is now imported (events.rs:32) and used; no unused
  import warning.
- Report contents: stall window, threshold, last-heartbeat age, pid, main-thread
  Rust id (fixed F3), OS id (Windows) or "unavailable" line, CPU line
  (Windows computed / "unavailable on this platform" elsewhere), episode
  counter, activity ring (oldest first, " (none recorded)" when empty). File
  naming `hang-<unix-ms>.txt` matches the plan. Best-effort write: failures
  eprintln + keep watching (watchdog.rs:202-217).

### Stall state machine / ring / report writer (watchdog.rs tests)

`stall_tracker_fires_once_per_episode_and_rearms` (fires at >=threshold,
quiet while stale, re-arm on <threshold, fires again), `activity_ring_evicts_oldest_above_capacity`,
`report_formatting_carries_the_evidence` (all key lines + oldest-first
ordering), `report_writer_creates_dir_and_file` — all correct and passing
against the implementation. `tempfile` is a dep of src-tauri (Cargo.toml),
available in tests.

### Multi-platform neutrality

- Every Windows-only bit is inside `#[cfg(windows)]`: `GetCurrentThreadId`
  (watchdog.rs:255-257), the `main_thread_os_id` param + report field
  (276-277, 308, 378-381), `times_at_last_heartbeat`/`prev_hb_ms`
  (317-320), `sample_times` (345-346), the busy-% computation (354-367) with
  the non-Windows fallback string (368-369), and `thread_times_ms` /
  `filetime_to_ms` (416-463). The E0658 fix keeps the two assignments in one
  cfg'd block. The non-Windows build compiles and the report degrades
  gracefully ("unavailable on this platform").
- `windows-sys` is declared under `[target.'cfg(windows)'.dependencies]`
  (src-tauri/Cargo.toml) with exactly the two features used
  (`Win32_Foundation`, `Win32_System_Threading`); Cargo.lock gained only the
  one expected edge. The lib crate's existing cfg entry is untouched.
- `run_on_main_thread` is a cross-platform Tauri API (exists on macOS too);
  the console changes (embeddings.rs, console.rs) are plain cross-platform
  Rust with no Windows assumptions. The one `eprintln` in the console path
  (`error: bundled model local load failed`) is cross-platform.

### Constitution

- Doc comments on every new pub item: `Watchdog` + `start`/`start_with_threshold`/
  `note`/`stop`, `ActivityRing` + `new`/`push`/`snapshot`, `StallTracker` +
  `new`/`observe`, `spawn_startup_load`, `spawn_startup_load_console`,
  `run_startup_load` (private but documented), `IpcState.watchdog`,
  `Brain.memory_store`/`embedder_status`/`pending_model_load` visibility
  changes (docs added), `EmbedderStartupPlan::UseHashThenLocalLoad`,
  `build_brain`/`build_brain_inner` doc updates.
- No `#[allow(...)]` anywhere in the diff.
- Regression tests accompany the behavioral fixes: F1's rename of
  `embedder_startup_plan_installed_model_defers_local_load` (asserts the new
  variant), F4's two watchdog_note test functions. The E0658 fix is a
  compile-level fix; the green build under `#![deny(warnings)]` is its gate.
- No dead code introduced (the `episodes` Arc is the only nit, finding 2);
  the `let _ =` swallows on `run_on_main_thread`/`emit` are intentional
  (best-effort) and match surrounding style.

## Suggested fix priority

1. Finding 1 (Low, race) — add the model-id guard (or an explicit doc-comment
   on the accepted trade-off) in `install_embedder`/`run_startup_load`.
2. Finding 2 (nit) — drop the `Arc` wrapper on `episodes`.
3. Finding 3 (nit) — one sentence in the embeddings module doc.
