# Review: Hang watchdog + main-thread blocking audit (plan 7fb6d24c)

Reviewed 2026-08-20 against `git diff HEAD` (9 modified files + untracked
`src-tauri/src/watchdog.rs`, plan files, and the research report). Verdict:
**2 correctness findings (1 high, 1 medium), 1 low correctness finding, 1
test-coverage nit, 1 documentation nit.** The watchdog core (state machine,
ring, report writer), the cfg(windows) gating, the Exit stop ordering, the
report dir choice, the embedder deferral safety (status transitions,
re-embed skip, hash-destroy-vector protection), and the build/test claims
were all verified — clean.

---

## Findings

### 1. HIGH (correctness, regression): `-console` mode silently loses the bundled embedder for installed models

`src/provider/client_factory.rs:314-320` (variant doc) + `:347` replaced
`UseBundledNow` with `UseHashThenLocalLoad` in `embedder_startup_plan`, and
`src-tauri/src/main.rs:476-485` spawns the deferred local load **only in the
Tauri setup hook** (`pending_model_load` is consumed nowhere else — grep
confirms). `build_brain` is shared by console mode
(`src-tauri/src/console.rs:1352`), and the console path never calls
`spawn_startup_load`.

- Before: console mode with a configured + installed bundled model loaded it
  synchronously (`build_embedder` → `BundledEmbedder::new`), status `Ready`.
- After: console mode builds the hash embedder, sets status `Checking`, and
  nothing ever flips it — semantic memory is silently degraded to keyword
  hashing for the whole session, and `embedder_status` stays `Checking`
  forever. This is the steady-state daily case (installed model), not a
  first-run corner, so it is a real regression introduced by this diff. (The
  pre-existing first-run *download* path has the same console gap, but it
  predates this change; the local-load deferral is new.)

Fix: extract the load+swap body of `spawn_startup_load`
(`src-tauri/src/ipc/embeddings.rs:138-179`) behind the `app.emit` calls into
a shared helper, and spawn it from `run_console` too — or keep console mode
on the synchronous `build_embedder` path (e.g. thread the deferral decision
per-frontend). Either way, add a note to `console.rs`'s brain comment.

### 2. Medium, correctness — CPU-time enrichment measures the wrong window

`src-tauri/src/watchdog.rs:309-310` samples the main-thread CPU baseline
once at watchdog start; `:340-349` then reports the **cumulative** kernel+user
delta divided by the **full watchdog-lifetime** wall time
(`now_ms = start.elapsed()`, `:331`). The comment at `:336-338` claims the
number "discriminates a busy loop (CPU ≈ 100%) from a blocked thread
(CPU ≈ 0%)" — but that is only true over the *stall window*. A 10 s busy-stall
inside a one-hour session reports ≈0.3% busy, indistinguishable from a
blocked thread, defeating the report's core diagnostic purpose.

Fix: capture the thread-times snapshot alongside each heartbeat (when the
heartbeat is observed to advance) and diff fire-time samples against the
*last heartbeat* sample, dividing by the stall window (`stall_ms`). The stall
window is already computed at `:333`.

### 3. Low (correctness, report accuracy) — `main_thread_rust_id` is the watchdog thread's id

`src-tauri/src/watchdog.rs:362` runs `std::thread::current().id()` inside
`detector_loop` — on the `"hang-watchdog"` thread (`:294-296`), not the main
thread. The doc comment on `start` (`:234-236`) explicitly says the captured
thread identity is the main thread's, and the Windows OS-id path (`:363-364`)
is captured in `start` where that's true — but the Rust id is not. On
macOS/Linux the report's only thread-identity field is therefore wrong.

Fix: capture `std::thread::current().id()` in `start` (like the
`cfg(windows)` OS id at `:252-254`) and pass it into `detector_loop`, or drop
the field.

### 4. Nit (test coverage) — `watchdog_note` has no unit test

`src-tauri/src/ipc/events.rs:50-75` — the new pure match on
`SerializableAgentEvent` (including the "streaming events are skipped" rule)
is untested, while every other new pure piece in this plan got a test
(StallTracker, ActivityRing, format_report, write_report,
`embedder_startup_plan_installed_model_defers_local_load`). A table test over
one event of each arm (Started/Finished/Error×2/WorkflowStateChanged/
Approval/UserQuestion/PromptDispatched/SkillStarted/Exited + a delta variant
asserting `None`) would close the gap cheaply.

### 5. Low (documentation sync) — hang reports are not documented anywhere user-visible

The constitution asks the reviewer to check README.md/PLAN.md for stale or
missing docs. The new watchdog writes `hang-*.txt` into the project's
`.coding/logs/` (and `%TEMP%\mnemo-hang-reports\` on the NeedsProject/Err
paths) and prints startup timing to stderr — a genuinely user-visible
diagnostics surface (a user will find these files in their repo and wonder).
README.md's "Key features" / PLAN.md's decisions table mention neither the
watchdog nor the report location. Module docs (watchdog.rs), the
`build_brain` timing doc, and the `EmbedderStartupPlan` docs are all up to
date — only the top-level user docs are missing a one-liner. Low severity
(no stale *incorrect* docs were found; this is an omission).

---

## Verified clean (no findings)

- **StallTracker / ActivityRing / report writer** — logic + tests correct;
  once-per-episode semantics, re-arm on fresh heartbeat, eviction order.
- **Heartbeat design vs. false stalls** — a 10 s main thread that cannot pump
  the event loop *is* a user-visible hang (Windows declares "Not responding"
  at ~5 s); a busy IPC handler that blocks pumping for 10 s deserves the
  report, and the CPU line is intended to disambiguate (see Finding 2 for
  the math fix). No starvation-by-design issue.
- **Races/deadlocks** — no Arc cycle (detector thread owns clones; `start`
  returns the outer `Arc`); the ring mutex is never held across I/O; the
  detector only checks `stop` + sleeps ≤500 ms after `stop()`; Mutex poison
  via `expect` matches project style; `app.state::<IpcState>()` in the
  forwarder cannot panic — state-changing events cannot precede `app.manage`
  (the main agent's pre-manage `ContextUsage` note maps to `None` in
  `watchdog_note`).
- **RunEvent::Exit ordering** — stop-first is correct; the deliberately
  blocking teardown (up to 10 s) must not be misreported as a stall.
- **Report dir** — Ready branch uses `backlog_root/.coding/logs` (project
  root per main.rs:243-246); NeedsProject/Err use the temp-dir fallback;
  `write_report` creates the dir. Correct.
- **Embedder deferral safety** — status transitions `Checking → Ready/Failed`
  mirror the download path; the cross-machine re-embed block is correctly
  skipped when `pending_load.is_some()` (main.rs:1140-1144) so the interim
  hash embedder can never destroy stored semantic vectors; `install_embedder`
  re-embed-skip fingerprint conditions are identical to the download path;
  `spawn_startup_load` mirrors `spawn_download` minus the progress poller and
  `Downloading` status (both correctly omitted — no network).
- **Multi-platform** — `windows-sys` is declared under
  `[target.'cfg(windows)'.dependencies]` (src-tauri/Cargo.toml:32-37, correct
  feature list Win32_Foundation + Win32_System_Threading); all Windows-only
  code (GetCurrentThreadId at :252-254, `thread_times_ms`, `filetime_to_ms`,
  the `main_thread_os_id` param + report fields) is `#[cfg(windows)]`-gated
  with graceful degradation in the report and in the module docs; the
  detector/ring/format/write path is pure cross-platform Rust; `run_on_main_thread`
  is a platform-neutral Tauri API. Non-Windows builds should compile.
- **Constitution** — every new `pub` item in watchdog.rs has a doc comment;
  no `#[allow(...)]` hacks anywhere in the diff; the new logic has tests
  (with the Finding 4 exception); no Windows-only APIs leak outside the
  sanctioned gate.
- **Build/test claims** — the diff itself is consistent with the stated
  green status (root + src-tauri tests, clean build under `#![deny(warnings)]`).

## Suggested fix priority

1. Console embedder regression (Finding 1) — user-facing silent degradation.
2. CPU window math (Finding 2) — the report's central discriminator.
3. Main-thread Rust id (Finding 3), watchdog_note test (Finding 4), README
   note (Finding 5).
