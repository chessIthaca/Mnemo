// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Hang watchdog — detects a stalled main thread and writes an evidence
//! report so the next AppHangB1-style freeze is diagnosable.
//!
//! # Why this exists
//!
//! The app's recurring "crashes" (2026-08-20, Windows `AppHangB1` event 1002)
//! are UI hangs: the process stays alive but the main thread stops pumping
//! messages, Windows offers "Close the program", and WER captures no stack
//! because the app records nothing itself. Panics print to stderr only and
//! `.coding/logs/*.jsonl` only mirrors provider requests, so a hang leaves no
//! app-side trace. This watchdog closes that gap.
//!
//! The watchdog's named stack capture (SymInitialize fix, 2026-08-20) turned
//! the recurring hang into a root cause: three byte-identical stacks showed a
//! tao 0.35.3 keyboard self-deadlock — `public_window_callback` holds
//! `KEY_EVENT_BUILDERS` while `KeyEventBuilder::process_message` calls
//! `PeekMessageW`, which dispatches an inbound SEND that re-enters the window
//! proc on the same thread and re-locks the non-reentrant `parking_lot`
//! mutex. Fixed by the vendored tao 0.35.4 backport of upstream PR #1215
//! (`vendor/tao/PATCHES.md`) + Rust-side delta coalescing in the event
//! forwarder (`DeltaBatcher`, `ipc/events.rs`). The watchdog stays armed: any
//! recurrence yields a named stack again.
//!
//! # How it works
//!
//! A detector thread pings the main thread every [`PING_INTERVAL`] by
//! scheduling a tiny heartbeat callback through
//! [`tauri::AppHandle::run_on_main_thread`] (a platform-neutral Tauri API —
//! it exists on every OS Tauri supports). If the main thread stops running
//! those callbacks for [`DEFAULT_STALL_THRESHOLD`] or longer, the detector
//! fires **once per stall episode** and writes `hang-<unix-ms>.txt` into the
//! report dir (`.coding/logs`). The report carries: stall duration, process +
//! main-thread ids, a ring of recent agent activity (fed by the event
//! forwarder), and — on Windows — the main thread's kernel/user CPU time
//! delta (discriminates a busy loop from a blocked thread), a **stack walk**
//! of the main thread at fire time (via `StackWalk64`, naming the exact
//! frame), and a **wait-chain summary** (via WCT) naming the object the main
//! thread is blocked on. The next 0%-CPU hang therefore names its line.
//!
//! # Multi-platform neutrality
//!
//! The detector, state machine, activity ring, and report format are pure
//! cross-platform Rust. Only the *enrichment* (thread OS id, CPU times,
//! stack capture, wait chain) is `cfg(windows)`-gated, because the
//! equivalents are entirely different APIs on other OSes (Mach
//! `thread_info` on macOS); on those platforms the report degrades
//! gracefully to the heartbeat + activity data with an explicit
//! "unavailable on this platform" note. A macOS enrichment can be added
//! later without touching the core.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::AppHandle;

/// How often the detector thread pings the main thread. Half a second keeps
/// heartbeat overhead negligible while bounding the report's stall precision.
const PING_INTERVAL: Duration = Duration::from_millis(500);

/// How far the detector's own tick may drift past [`PING_INTERVAL`] before the
/// gap is read as "this process was not running" rather than "the main thread
/// is stuck".
///
/// The detector sleeps 500 ms per tick. If a tick lands 10 s late, the
/// DETECTOR was descheduled too — machine sleep/hibernate, or system-wide
/// starvation. Nothing can be inferred about the main thread across that
/// window: it never got to run either, and the heartbeat is stale purely
/// because time passed. Reporting it as a stall produced the 12,068,773 ms
/// (3.35 h) "hang" in hang-1787555320634.txt, which was an overnight sleep.
const SUSPEND_GAP_MS: u64 = 10_000;

/// Default main-thread stall threshold: the detector fires when no heartbeat
/// has landed for this long. Well above any legitimate main-thread hiccup and
/// below Windows' ~5 s "not responding" heuristics once user reaction time is
/// included.
pub const DEFAULT_STALL_THRESHOLD: Duration = Duration::from_secs(10);

/// How many activity notes the ring keeps (enough to reconstruct the last
/// minute of agent activity at one note per event).
const RING_CAPACITY: usize = 64;

/// A fixed-capacity ring of recent activity notes, newest last.
///
/// Fed by [`Watchdog::note`] from the agent-event forwarder; drained by the
/// report writer when a stall fires. The mutex is only ever held for a
/// push/pop/snapshot — no lock is held across I/O.
pub struct ActivityRing {
    entries: Mutex<VecDeque<String>>,
    cap: usize,
}

impl ActivityRing {
    /// Create a ring that keeps at most `cap` entries.
    pub fn new(cap: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(cap)),
            cap,
        }
    }

    /// Append a note, evicting the oldest entry when over capacity.
    pub fn push(&self, entry: impl Into<String>) {
        let mut entries = self.entries.lock().expect("activity ring poisoned");
        entries.push_back(entry.into());
        while entries.len() > self.cap {
            entries.pop_front();
        }
    }

    /// A copy of the current entries, oldest first.
    pub fn snapshot(&self) -> Vec<String> {
        self.entries
            .lock()
            .expect("activity ring poisoned")
            .iter()
            .cloned()
            .collect()
    }
}

/// Pure stall state machine: fires exactly once per stall episode.
///
/// A stall episode is the whole time the main-thread heartbeat stays stale.
/// The detector reports the FIRST time the stall crosses the threshold, then
/// stays quiet until a heartbeat proves the main thread is alive again
/// (which re-arms it for the next episode).
#[derive(Debug)]
pub struct StallTracker {
    threshold: Duration,
    armed: bool,
}

impl StallTracker {
    /// Create a tracker that fires at `threshold` of staleness.
    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            armed: true,
        }
    }

    /// Observe the current stall duration; returns `true` exactly once per
    /// stall episode (the moment the threshold is first crossed).
    /// Re-arm without observing a heartbeat.
    ///
    /// Used when the measurement window is discarded rather than satisfied
    /// (a suspend/resume gap): the next genuine stall must still fire, so the
    /// tracker cannot be left disarmed.
    pub fn rearm(&mut self) {
        self.armed = true;
    }

    pub fn observe(&mut self, stall: Duration) -> bool {
        if self.armed {
            if stall >= self.threshold {
                self.armed = false;
                return true;
            }
        } else if stall < self.threshold {
            // A fresh heartbeat landed — the main thread is alive again.
            self.armed = true;
        }
        false
    }
}

/// Everything the report writer needs, pre-formatted so [`format_report`]
/// stays platform-neutral and unit-testable.
struct ReportData {
    /// Unix milliseconds when the stall fired (also the report file name).
    fired_ms: u64,
    /// Stall duration at fire time.
    stall_ms: u64,
    /// The configured stall threshold.
    threshold_ms: u64,
    /// Milliseconds between watchdog start and the last heartbeat.
    last_heartbeat_ms: u64,
    /// OS process id.
    pid: u32,
    /// The main thread's Rust id (`std::thread::ThreadId` debug form).
    main_thread_rust_id: String,
    /// The main thread's OS id (Windows only — `None` elsewhere).
    main_thread_os_id: Option<u32>,
    /// Stall episode number (1-based).
    episode: u64,
    /// Pre-formatted CPU enrichment line (Windows) or the "unavailable"
    /// fallback line (other platforms).
    cpu_line: String,
    /// Main-thread stack frames at fire time (Windows; empty elsewhere).
    stack_lines: Vec<String>,
    /// Wait-chain summary lines naming the waited object (Windows; empty
    /// elsewhere).
    wait_chain_lines: Vec<String>,
    /// Recent activity notes, oldest first.
    activity: Vec<String>,
}

/// Render the evidence report body.
fn format_report(d: &ReportData) -> String {
    let mut out = String::new();
    out.push_str("mnemo hang watchdog — main thread stalled\n");
    out.push_str("==========================================\n");
    out.push_str(&format!("fired at (unix ms): {}\n", d.fired_ms));
    out.push_str(&format!(
        "stall at fire: {} ms (threshold {} ms)\n",
        d.stall_ms, d.threshold_ms
    ));
    out.push_str(&format!(
        "last heartbeat: {} ms after watchdog start\n",
        d.last_heartbeat_ms
    ));
    out.push_str(&format!("episode: {}\n", d.episode));
    out.push_str(&format!("process id: {}\n", d.pid));
    out.push_str(&format!("main thread rust id: {}\n", d.main_thread_rust_id));
    match d.main_thread_os_id {
        Some(id) => out.push_str(&format!("main thread os id: {id}\n")),
        None => out.push_str("main thread os id: unavailable on this platform\n"),
    }
    out.push_str(&format!("{}\n", d.cpu_line));
    out.push_str("main thread wait chain:\n");
    if d.wait_chain_lines.is_empty() {
        out.push_str("  (unavailable on this platform)\n");
    } else {
        for note in &d.wait_chain_lines {
            out.push_str(&format!("  {note}\n"));
        }
    }
    out.push_str("main thread stack at fire:\n");
    if d.stack_lines.is_empty() {
        out.push_str("  (unavailable on this platform)\n");
    } else {
        for frame in &d.stack_lines {
            out.push_str(&format!("  {frame}\n"));
        }
    }
    out.push_str("recent activity (oldest first):\n");
    if d.activity.is_empty() {
        out.push_str("  (none recorded)\n");
    } else {
        for note in &d.activity {
            out.push_str(&format!("  {note}\n"));
        }
    }
    out
}

/// Write the report into `dir` (created if missing) as
/// `hang-<unix-ms>.txt`. Best-effort: on failure the detector logs and keeps
/// watching — a report that can't be written must never take the watchdog
/// down.
fn write_report(dir: &Path, data: &ReportData) -> std::io::Result<PathBuf> {
    let content = format_report(data);
    let path = dir.join(format!("hang-{}.txt", data.fired_ms));
    let result = (|| {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, content)
    })();
    match &result {
        Ok(()) => eprintln!(
            "mnemo: HANG DETECTED — main thread stalled {} ms; evidence written to {}",
            data.stall_ms,
            path.display()
        ),
        Err(e) => eprintln!("mnemo: hang report write failed ({}): {e}", path.display()),
    }
    result.map(|_| path)
}

/// The hang watchdog — see the [module docs](self) for the design.
///
/// This is the app-facing control surface: [`note`](Self::note) feeds the
/// activity ring and [`stop`](Self::stop) shuts the detector down. All
/// detector internals (heartbeat, timers, report path) are owned by the
/// detector thread spawned in [`Watchdog::start`].
pub struct Watchdog {
    /// Set by [`Watchdog::stop`]; the detector thread checks it every ping.
    stop: Arc<AtomicBool>,
    /// Recent activity notes fed by [`Watchdog::note`].
    ring: Arc<ActivityRing>,
}

impl Watchdog {
    /// Start the watchdog with the default stall threshold. Must be called
    /// from the main thread (setup), so the OS thread id it captures really
    /// is the main thread's.
    pub fn start(app: AppHandle, report_dir: PathBuf) -> Arc<Watchdog> {
        Self::start_with_threshold(app, report_dir, DEFAULT_STALL_THRESHOLD)
    }

    /// Start the watchdog with a custom stall threshold (tests use a short
    /// one; production always uses [`DEFAULT_STALL_THRESHOLD`]).
    pub fn start_with_threshold(
        app: AppHandle,
        report_dir: PathBuf,
        stall_threshold: Duration,
    ) -> Arc<Watchdog> {
        let heartbeat_ms = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(ActivityRing::new(RING_CAPACITY));
        let start = Instant::now();
        // The main thread's Rust id, captured here in `start` (which runs on
        // the main thread) — the detector thread's own id would be useless.
        let main_thread_rust_id = format!("{:?}", std::thread::current().id());
        #[cfg(windows)]
        let main_thread_os_id =
            unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
        let watchdog = Arc::new(Watchdog {
            stop: stop.clone(),
            ring: ring.clone(),
        });
        ring.push("watchdog started");

        std::thread::Builder::new()
            .name("hang-watchdog".to_string())
            .spawn(move || {
                detector_loop(
                    app,
                    heartbeat_ms,
                    stop,
                    ring,
                    start,
                    stall_threshold,
                    report_dir,
                    main_thread_rust_id,
                    #[cfg(windows)]
                    main_thread_os_id,
                );
            })
            .expect("hang watchdog thread spawn must succeed");
        watchdog
    }

    /// Record an agent-activity note (fed from the event forwarder). Cheap
    /// and non-blocking: the ring mutex is held only for the push.
    pub fn note(&self, activity: impl Into<String>) {
        self.ring.push(activity);
    }

    /// Stop the detector thread (idempotent). Call in `RunEvent::Exit`
    /// before the exit teardown so the watchdog never observes the app's
    /// own shutdown as a stall.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The detector thread: ping → observe → (once per episode) report.
fn detector_loop(
    app: AppHandle,
    heartbeat_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    ring: Arc<ActivityRing>,
    start: Instant,
    stall_threshold: Duration,
    report_dir: PathBuf,
    main_thread_rust_id: String,
    #[cfg(windows)] main_thread_os_id: u32,
) {
    let mut tracker = StallTracker::new(stall_threshold);
    // Episode counter — owned by this thread only (the report carries it per
    // fire; nothing else reads it).
    let mut episodes = 0u64;
    // Main-thread CPU times as of the last heartbeat that was observed to
    // advance. A stall diffs its fire-time sample against this, so the
    // reported busy-% describes the STALL WINDOW (heartbeat → fire), not the
    // whole watchdog lifetime — a 10 s busy stall an hour into a session must
    // still read ≈100% (Windows only; `None` when sampling failed).
    #[cfg(windows)]
    let mut times_at_last_heartbeat = thread_times_ms(main_thread_os_id);
    #[cfg(windows)]
    let mut prev_hb_ms: u64 = 0;
    // When the previous tick ran, so a late tick can be told apart from a
    // stalled main thread (see `SUSPEND_GAP_MS`).
    let mut prev_tick_ms: u64 = start.elapsed().as_millis() as u64;

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(PING_INTERVAL);
        if stop.load(Ordering::Relaxed) {
            return;
        }

        // Ping the main thread: the callback only runs when the main thread
        // pumps its event loop again, so a stale heartbeat means a stall.
        let hb = heartbeat_ms.clone();
        let _ = app.run_on_main_thread(move || {
            hb.store(
                Instant::now().duration_since(start).as_millis() as u64,
                Ordering::Relaxed,
            );
        });

        let now_ms = start.elapsed().as_millis() as u64;

        // Did the DETECTOR just lose the CPU? A tick far later than its own
        // ping interval means the whole process was suspended (sleep,
        // hibernate, or a hard system stall). The main thread did not get to
        // run during that window either, so its stale heartbeat says nothing
        // about liveness — measuring across the gap would report the sleep
        // itself as a multi-hour hang. Restart the measurement from here.
        let tick_gap_ms = now_ms.saturating_sub(prev_tick_ms);
        prev_tick_ms = now_ms;
        if tick_gap_ms > SUSPEND_GAP_MS {
            heartbeat_ms.store(now_ms, Ordering::Relaxed);
            tracker.rearm();
            ring.push(format!(
                "watchdog: {tick_gap_ms} ms gap between ticks (suspend/resume) \
                 — stall measurement restarted"
            ));
            #[cfg(windows)]
            {
                times_at_last_heartbeat = thread_times_ms(main_thread_os_id);
                prev_hb_ms = now_ms;
            }
            continue;
        }

        let last_hb_ms = heartbeat_ms.load(Ordering::Relaxed);
        let stall_ms = now_ms.saturating_sub(last_hb_ms);

        #[cfg(windows)]
        let sample_times = thread_times_ms(main_thread_os_id);

        if tracker.observe(Duration::from_millis(stall_ms)) {
            episodes += 1;
            let episode = episodes;
            // CPU-time enrichment: diff the fire-time sample against the
            // sample at the last observed heartbeat and divide by the stall
            // window itself. Busy ≈ 100% means a busy loop; ≈ 0% means the
            // thread is blocked (lock, I/O, or a wedged native call).
            #[cfg(windows)]
            let cpu_line = match (times_at_last_heartbeat, sample_times) {
                (Some((base_kernel, base_user)), Some((now_kernel, now_user))) => {
                    let cpu_ms =
                        now_kernel.saturating_sub(base_kernel) + now_user.saturating_sub(base_user);
                    let wall_ms = stall_ms.max(1);
                    let pct = (cpu_ms as f64 / wall_ms as f64) * 100.0;
                    format!(
                        "main thread cpu over the stall window: {pct:.1}% busy \
                         ({cpu_ms} ms kernel+user over {wall_ms} ms wall)"
                    )
                }
                _ => String::from("main thread cpu: unavailable (could not sample thread times)"),
            };
            #[cfg(not(windows))]
            let cpu_line = String::from("main thread cpu: unavailable on this platform");

            // Stack + wait-chain enrichment (Windows): suspend the main
            // thread, walk its stack, resume it on every path, and ask WCT
            // what it is blocked on. The thread is already stalled — the
            // capture must never leave it suspended, and best-effort
            // failures degrade to an empty section.
            #[cfg(windows)]
            let (stack_lines, wait_chain_lines) = {
                let frames = capture_thread_stack(main_thread_os_id);
                let chain = capture_wait_chain(main_thread_os_id);
                (frames, chain)
            };
            #[cfg(not(windows))]
            let (stack_lines, wait_chain_lines) = (Vec::new(), Vec::new());

            let data = ReportData {
                fired_ms: unix_millis(),
                stall_ms,
                threshold_ms: stall_threshold.as_millis() as u64,
                last_heartbeat_ms: last_hb_ms,
                pid: std::process::id(),
                main_thread_rust_id: main_thread_rust_id.clone(),
                #[cfg(windows)]
                main_thread_os_id: Some(main_thread_os_id),
                #[cfg(not(windows))]
                main_thread_os_id: None,
                episode,
                cpu_line,
                stack_lines,
                wait_chain_lines,
                activity: ring.snapshot(),
            };
            let _ = write_report(&report_dir, &data);
        }

        // The heartbeat advanced this round → the main thread ran at some
        // point since the last check. Roll the CPU baseline forward so the
        // next stall's busy-% measures only its own quiet window. (One
        // cfg-gated block — a bare cfg on the assignment statement itself
        // would be attributes-on-expressions, E0658.)
        #[cfg(windows)]
        {
            if last_hb_ms > prev_hb_ms {
                times_at_last_heartbeat = sample_times;
            }
            prev_hb_ms = last_hb_ms;
        }
    }
}

/// Unix milliseconds now (wall clock, for report file names + correlation
/// with WER/app-log timestamps).
fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Windows-only: the main thread's cumulative kernel + user CPU time in
/// milliseconds (a single sample; the detector diffs against its startup
/// baseline). Returns `None` when the thread handle can't be opened.
#[cfg(windows)]
fn thread_times_ms(thread_os_id: u32) -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{
        GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, thread_os_id);
        if handle.is_null() {
            return None;
        }
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let ok = GetThreadTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some((filetime_to_ms(kernel), filetime_to_ms(user)))
    }
}

/// Convert a `FILETIME` (100 ns units since 1601) to milliseconds.
#[cfg(windows)]
fn filetime_to_ms(ft: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    let raw = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
    raw / 10_000
}

// ---------------------------------------------------------------------------
// Windows-only stack + wait-chain capture (see the module docs §How it works)
// ---------------------------------------------------------------------------

// windows-sys is a cfg(windows)-only dependency, so every import and item in
// this section must be gated the same way — an ungated `use windows_sys::…`
// breaks macOS/Linux builds at resolve time (review H1).
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::Debug::{
    GetThreadContext, StackWalk64, SymFromAddr, SymGetLineFromAddr64, SymGetModuleBase64,
    SymInitialize, SymSetOptions, CONTEXT, CONTEXT_FULL_AMD64, IMAGEHLP_LINE64, STACKFRAME64,
    SYMBOL_INFO, SYMOPT_DEFERRED_LOADS, SYMOPT_LOAD_LINES, SYMOPT_UNDNAME,
};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
#[cfg(windows)]
use windows_sys::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_AMD64;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT,
    THREAD_SUSPEND_RESUME,
};

/// How many stack frames the walk records (a deep FFI/compositor stack can
/// run to hundreds; 64 frames is plenty to name the blocking call).
#[cfg(windows)]
const MAX_STACK_FRAMES: usize = 64;

/// Owns a suspended thread handle; drop resumes the thread and closes the
/// handle — the main thread is already stalled and must never be left
/// suspended by the capture (review L1: the close used to leak one HANDLE
/// per capture).
#[cfg(windows)]
struct ThreadGuard(HANDLE);

#[cfg(windows)]
impl Drop for ThreadGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ResumeThread(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// Capture a stack walk of `thread_os_id` (the stalled main thread),
/// resolving each frame to `module!symbol` (with `+0xN` displacement) or a
/// `module+0xRVA` fallback, and appending `(line)` when line info is
/// available. Best-effort: any failure yields an empty vec, never a panic.
///
/// The thread is briefly suspended while the context is read; the resume
/// (and handle close) is guaranteed by [`ThreadGuard`]'s drop. **Never call
/// this on the calling thread itself** — `SuspendThread` defers a
/// self-suspend until the thread returns to the kernel, so the next syscall
/// would deadlock it before the resume guard runs. `dbghelp`'s symbol store
/// is initialized once per process via a `OnceLock` (cheap, deferred loads,
/// no symbol server).
///
/// One real-world wrinkle: a thread suspended *inside* a syscall is frozen
/// in kernel mode and `GetThreadContext` fails with `ERROR_PARTIAL_COPY`
/// (299) until the thread is resumed and quiesces. The capture therefore
/// retries the whole suspend→context→walk cycle a few times — between
/// attempts the target is resumed (by the drop guard) and briefly left to
/// settle, which lets a mid-syscall thread reach user mode. A thread that
/// never leaves an indefinite kernel wait degrades to an empty section; the
/// report still carries CPU% and the wait chain.
#[cfg(windows)]
fn capture_thread_stack(thread_os_id: u32) -> Vec<String> {
    for _ in 0..3 {
        let frames = capture_thread_stack_once(thread_os_id);
        if !frames.is_empty() {
            return frames;
        }
        // Leave the target unsuspended between attempts so a mid-syscall
        // thread can quiesce before the next suspend.
        std::thread::sleep(Duration::from_millis(5));
    }
    Vec::new()
}

/// `CONTEXT` as declared by windows-sys carries only natural alignment (8),
/// but the x64 CONTEXT must be 16-byte aligned (`DECLSPEC_ALIGN(16)` in the
/// SDK headers) — `GetThreadContext` fails with `ERROR_NOACCESS` (998) on a
/// misaligned buffer, which makes stack capture flaky per binary layout.
/// This wrapper forces the required alignment.
#[cfg(windows)]
#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

/// One suspend → context → walk → resume attempt; see [`capture_thread_stack`].
#[cfg(windows)]
fn capture_thread_stack_once(thread_os_id: u32) -> Vec<String> {
    let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, 0, thread_os_id) };
    if handle.is_null() {
        return Vec::new();
    }
    // SuspendThread returns the previous suspend count (u32::MAX on error).
    if unsafe { SuspendThread(handle) } == u32::MAX {
        unsafe { CloseHandle(handle) };
        return Vec::new();
    }
    let _thread_guard = ThreadGuard(handle);

    let mut aligned = AlignedContext(unsafe { std::mem::zeroed() });
    let context = &mut aligned.0;
    // CONTEXT_FULL, not CONTEXT_CONTROL. `StackWalk64` unwinds x64 frames
    // through the function tables (RUNTIME_FUNCTION / UNWIND_INFO), and the
    // unwind codes there restore the NON-VOLATILE INTEGER registers — rbx,
    // rsi, rdi, r12-r15. CONTEXT_CONTROL supplies only rip/rsp/rbp/eflags, so
    // those arrive zeroed and the walk dies after a frame or two: every hang
    // report captured before 2026-08-24 shows exactly 2 of 64 frames, with
    // the CALLER — the only frame that identifies the deadlock — missing.
    context.ContextFlags = CONTEXT_FULL_AMD64;
    // ERROR_PARTIAL_COPY (299) is the documented failure when a thread is
    // suspended mid-syscall — the target stays suspended while we retry;
    // the outer attempt loop in [`capture_thread_stack`] is the escape
    // hatch for threads frozen inside an indefinite kernel wait.
    let mut ok = 0;
    for _ in 0..4 {
        ok = unsafe { GetThreadContext(handle, context) };
        if ok != 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if ok == 0 {
        return Vec::new();
    }

    let mut frame: STACKFRAME64 = unsafe { std::mem::zeroed() };
    frame.AddrPC.Offset = context.Rip;
    frame.AddrPC.Mode = windows_sys::Win32::System::Diagnostics::Debug::AddrModeFlat;
    frame.AddrFrame.Offset = context.Rbp;
    frame.AddrFrame.Mode = windows_sys::Win32::System::Diagnostics::Debug::AddrModeFlat;
    frame.AddrStack.Offset = context.Rsp;
    frame.AddrStack.Mode = windows_sys::Win32::System::Diagnostics::Debug::AddrModeFlat;

    let process = unsafe { GetCurrentProcess() };
    // dbghelp must be initialized before the walk so function-table access
    // (x64 unwind data) resolves; `resolve_frame` reuses the same
    // once-per-process setup.
    ensure_symbols_ready(process);
    let mut lines = Vec::new();
    for _ in 0..MAX_STACK_FRAMES {
        let ok = unsafe {
            StackWalk64(
                IMAGE_FILE_MACHINE_AMD64.into(),
                process,
                handle,
                &mut frame,
                context as *mut CONTEXT as *mut core::ffi::c_void,
                None,
                // x64 unwind requires the canonical dbghelp routines — with
                // None here the walk cannot resolve function tables and
                // bails out on the first frame.
                Some(windows_sys::Win32::System::Diagnostics::Debug::SymFunctionTableAccess64),
                Some(windows_sys::Win32::System::Diagnostics::Debug::SymGetModuleBase64),
                None,
            )
        };
        if ok == 0 || frame.AddrPC.Offset == 0 {
            break;
        }
        let address = frame.AddrPC.Offset;
        lines.push(resolve_frame(process, address));
        if frame.AddrReturn.Offset == 0 {
            break;
        }
    }
    lines
}

/// Initialize dbghelp's symbol store once per process (never torn down —
/// the process is either hung or exiting when the watchdog runs). Deferred
/// loads, undecorated names, line info; no symbol server.
#[cfg(windows)]
fn ensure_symbols_ready(process: HANDLE) {
    static SYM_READY: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    SYM_READY.get_or_init(|| unsafe {
        let _ = SymSetOptions(SYMOPT_DEFERRED_LOADS | SYMOPT_UNDNAME | SYMOPT_LOAD_LINES);
        // fInvadeProcess = TRUE. With FALSE, dbghelp starts with an EMPTY
        // module list: SymGetModuleBase64 then returns 0 for every address,
        // SymFunctionTableAccess64 finds no RUNTIME_FUNCTION, and StackWalk64
        // cannot unwind past the first frame — which is why every hang report
        // before 2026-08-24 carried 1-2 bare addresses and no symbol names at
        // all (the 2026-08-23 note had to hand-parse ntdll's export table to
        // identify them). Invading enumerates the loaded modules up front, so
        // both the unwind and the name resolution work.
        // Explicit search path = the running executable's own directory.
        // A NULL path leaves dbghelp on its defaults, which in practice does
        // not reliably pick up the PDB sitting next to a Rust binary — the
        // app's own frames then degrade to module+offset while only the
        // system DLLs resolve by name, and the app frames are precisely the
        // ones that identify a deadlock.
        let search_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
            .and_then(|d| std::ffi::CString::new(d).ok());
        let path_ptr = search_path
            .as_ref()
            .map(|s| s.as_ptr() as *const u8)
            .unwrap_or(std::ptr::null());
        let _ = SymInitialize(process, path_ptr, 1);
    });
}

/// The file name of the module containing `address` (e.g. `ntdll.dll`), or
/// `None` when the address belongs to no loaded module.
///
/// A frame is far more useful prefixed with its module: `ntdll.dll!…` versus a
/// bare offset says immediately whether the app is stuck in its own code, in
/// the CRT, or in the kernel-facing layer. The module base doubles as the
/// `HMODULE`, so the name comes straight from `GetModuleFileNameW` without
/// needing symbols to be present.
#[cfg(windows)]
fn module_name_for(process: HANDLE, address: u64) -> Option<(String, u64)> {
    let base = unsafe { SymGetModuleBase64(process, address) };
    if base == 0 {
        return None;
    }
    let mut buf = [0u16; 260];
    let len = unsafe {
        GetModuleFileNameW(
            base as *mut core::ffi::c_void,
            buf.as_mut_ptr(),
            buf.len() as u32,
        )
    };
    if len == 0 {
        return Some((format!("0x{base:x}"), address - base));
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    let name = full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string();
    Some((name, address - base))
}

/// Resolve one code address to a `module!symbol+0xN (file:line)` frame line.
/// Best-effort: symbol lookup first (via `SymFromAddr` into a
/// `SYMBOL_INFO`-sized buffer), then a `module+0xRVA` fallback using the
/// module base from `SymGetModuleBase64`.
#[cfg(windows)]
fn resolve_frame(process: HANDLE, address: u64) -> String {
    ensure_symbols_ready(process);

    let mut displacement: u64 = 0;
    // SYMBOL_INFO is a variable-length struct: the buffer must be
    // sizeof(SYMBOL_INFO) + room for the longest symbol name + NUL.
    const NAME_BUDGET: usize = 1024;
    let mut buf = [0u8; std::mem::size_of::<SYMBOL_INFO>() + NAME_BUDGET];
    let symbol = unsafe { &mut *(buf.as_mut_ptr() as *mut SYMBOL_INFO) };
    symbol.SizeOfStruct = std::mem::size_of::<SYMBOL_INFO>() as u32;
    symbol.MaxNameLen = NAME_BUDGET as u32;
    let has_symbol = unsafe { SymFromAddr(process, address, &mut displacement, symbol) };
    if has_symbol != 0 {
        let name = unsafe {
            let ptr = symbol.Name.as_ptr() as *const u8;
            let len = symbol.NameLen as usize;
            String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned()
        };
        let mut line = match module_name_for(process, address) {
            Some((module, _)) => format!("{module}!{name}+0x{displacement:x}"),
            None => format!("{name}+0x{displacement:x}"),
        };
        let mut line64: IMAGEHLP_LINE64 = unsafe { std::mem::zeroed() };
        line64.SizeOfStruct = std::mem::size_of::<IMAGEHLP_LINE64>() as u32;
        let mut line_displacement: u32 = 0;
        if unsafe { SymGetLineFromAddr64(process, address, &mut line_displacement, &mut line64) }
            != 0
            && !line64.FileName.is_null()
        {
            let file = unsafe { std::ffi::CStr::from_ptr(line64.FileName.cast()) };
            if let Ok(file) = file.to_str() {
                line.push_str(&format!(" ({file}:{})", line64.LineNumber));
            }
        }
        line
    } else {
        // No symbol (no PDB for that module) — the module name plus the
        // offset is still enough to identify the frame against a map file or
        // a disassembler, which a bare address is not.
        match module_name_for(process, address) {
            Some((module, offset)) => format!("{module}+0x{offset:x}"),
            None => format!("0x{address:x}"),
        }
    }
}

/// WCT bindings — `wct.h` declares these `WINADVAPI` (advapi32.dll); the
/// `windows-sys` 0.59 crate ships no WCT surface, so the three functions and
/// the node struct are hand-declared here, field-for-field from the SDK
/// header. `wct.dll` does not exist on modern Windows — the exports live in
/// advapi32.dll (verified at runtime).
///
/// ```c
/// typedef enum { WctCriticalSectionType=1, WctSendMessageType, WctMutexType,
///   WctAlpcType, WctComType, WctThreadWaitType, WctProcessWaitType,
///   WctThreadType, WctComActivationType, WctUnknownType, WctSocketIoType,
///   WctSmbIoType, WctMaxType } WCT_OBJECT_TYPE;
/// typedef enum { WctStatusNoAccess=1, WctStatusRunning, WctStatusBlocked,
///   WctStatusPidOnly, WctStatusPidOnlyRpcss, WctStatusOwned,
///   WctStatusNotOwned, WctStatusAbandoned, WctStatusUnknown, WctStatusError,
///   WctStatusMax } WCT_OBJECT_STATUS;
/// typedef struct _WAITCHAIN_NODE_INFO { WCT_OBJECT_TYPE ObjectType;
///   WCT_OBJECT_STATUS ObjectStatus; union { struct { WCHAR ObjectName[128];
///   LARGE_INTEGER Timeout; BOOL Alertable; } LockObject; struct { DWORD
///   ProcessId; DWORD ThreadId; DWORD WaitTime; DWORD ContextSwitches; }
///   ThreadObject; }; } WAITCHAIN_NODE_INFO;
/// #define WCT_MAX_NODE_COUNT 16
/// #define WCT_OBJNAME_LENGTH 128
/// ```
#[cfg(windows)]
mod wct {
    use windows_sys::Win32::Foundation::HANDLE;

    /// Object kinds a wait-chain node can describe (wct.h `WCT_OBJECT_TYPE`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(i32)]
    pub enum WctObjectType {
        CriticalSection = 1,
        SendMessage = 2,
        Mutex = 3,
        Alpc = 4,
        Com = 5,
        ThreadWait = 6,
        ProcessWait = 7,
        Thread = 8,
        ComActivation = 9,
        Unknown = 10,
        SocketIo = 11,
        SmbIo = 12,
    }

    /// Status of the waited object (wct.h `WCT_OBJECT_STATUS`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(i32)]
    pub enum WctObjectStatus {
        NoAccess = 1,
        Running = 2,
        Blocked = 3,
        PidOnly = 4,
        PidOnlyRpcss = 5,
        Owned = 6,
        NotOwned = 7,
        Abandoned = 8,
        Unknown = 9,
        Error = 10,
    }

    /// One node of a wait chain (wct.h `WAITCHAIN_NODE_INFO`). The union is
    /// modeled as two flat, overlapping views (the lock-object and
    /// thread-object arms never both matter for the same node) — a Rust
    /// `union` of the two structs would require unsafe field reads anyway.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WaitChainNodeInfo {
        pub object_type: i32,
        pub object_status: i32,
        pub lock_object_name: [u16; 128],
        pub lock_timeout: i64,
        pub lock_alertable: i32,
    }

    impl WaitChainNodeInfo {
        /// The node's object kind; values beyond the known range (only
        /// ≤ WctMaxType occurs in practice) map to [`WctObjectType::Unknown`].
        pub fn object_type(&self) -> WctObjectType {
            match self.object_type {
                1 => WctObjectType::CriticalSection,
                2 => WctObjectType::SendMessage,
                3 => WctObjectType::Mutex,
                4 => WctObjectType::Alpc,
                5 => WctObjectType::Com,
                6 => WctObjectType::ThreadWait,
                7 => WctObjectType::ProcessWait,
                8 => WctObjectType::Thread,
                9 => WctObjectType::ComActivation,
                11 => WctObjectType::SocketIo,
                12 => WctObjectType::SmbIo,
                _ => WctObjectType::Unknown,
            }
        }

        /// The node's status; values beyond the known range map to
        /// [`WctObjectStatus::Error`].
        pub fn object_status(&self) -> WctObjectStatus {
            match self.object_status {
                1 => WctObjectStatus::NoAccess,
                2 => WctObjectStatus::Running,
                3 => WctObjectStatus::Blocked,
                4 => WctObjectStatus::PidOnly,
                5 => WctObjectStatus::PidOnlyRpcss,
                6 => WctObjectStatus::Owned,
                7 => WctObjectStatus::NotOwned,
                8 => WctObjectStatus::Abandoned,
                9 => WctObjectStatus::Unknown,
                _ => WctObjectStatus::Error,
            }
        }

        /// The object name when this node is a lock object, trimmed to the
        /// WCHAR terminator if any.
        pub fn object_name(&self) -> String {
            let end = self
                .lock_object_name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(self.lock_object_name.len());
            String::from_utf16_lossy(&self.lock_object_name[..end])
        }
    }

    pub const WCT_MAX_NODE_COUNT: usize = 16;
    pub const WCT_OBJNAME_LENGTH: usize = 128;

    // Compile-time guard: the flat layout must be byte-identical to MSVC's
    // WAITCHAIN_NODE_INFO (8-byte header + 256-byte name + 8-byte timeout +
    // 4-byte alertable + 4-byte tail padding = 280). A mismatch would read
    // garbage (or past the buffer) in GetThreadWaitChain.
    const _: () = assert!(std::mem::size_of::<WaitChainNodeInfo>() == 280);

    /// A WCT session handle (wct.h `HWCT`).
    pub type WctSession = HANDLE;

    /// The async completion callback WCT can invoke (wct.h
    /// `PWAITCHAINCALLBACK`); only the synchronous session is used here, so
    /// the alias exists purely to mirror the header's signature.
    pub type WaitChainCallback = Option<
        unsafe extern "system" fn(
            WctSession,
            usize,
            u32,
            *mut u32,
            *mut WaitChainNodeInfo,
            *mut i32,
        ),
    >;

    #[link(name = "AdvAPI32")]
    extern "system" {
        pub fn OpenThreadWaitChainSession(flags: u32, callback: WaitChainCallback) -> WctSession;
        pub fn GetThreadWaitChain(
            handle: WctSession,
            context: usize,
            flags: u32,
            thread_id: u32,
            node_count: *mut u32,
            node_info: *mut WaitChainNodeInfo,
            is_cycle: *mut i32,
        ) -> i32;
        pub fn CloseThreadWaitChainSession(handle: WctSession);
    }
}

/// Capture a wait-chain summary for `thread_os_id` (the stalled main thread):
/// what object is it waiting on, in what state, and who holds it. Returns
/// human-readable lines, or an empty vec when WCT is unavailable (e.g. on
/// pre-Win7 systems or when the session cannot be opened).
#[cfg(windows)]
fn capture_wait_chain(thread_os_id: u32) -> Vec<String> {
    use wct::{GetThreadWaitChain, WaitChainNodeInfo, WCT_MAX_NODE_COUNT, WCT_OBJNAME_LENGTH};

    unsafe {
        let session = wct::OpenThreadWaitChainSession(0, None);
        if session.is_null() {
            return Vec::new();
        }
        let mut node_count: u32 = WCT_MAX_NODE_COUNT as u32;
        let mut nodes = [WaitChainNodeInfo {
            object_type: 0,
            object_status: 0,
            lock_object_name: [0u16; WCT_OBJNAME_LENGTH],
            lock_timeout: 0,
            lock_alertable: 0,
        }; WCT_MAX_NODE_COUNT];
        let mut is_cycle: i32 = 0;
        let ok = GetThreadWaitChain(
            session,
            0,
            0,
            thread_os_id,
            &mut node_count,
            nodes.as_mut_ptr(),
            &mut is_cycle,
        );
        wct::CloseThreadWaitChainSession(session);
        if ok == 0 || node_count == 0 {
            return Vec::new();
        }

        let mut lines: Vec<String> = Vec::new();
        for node in nodes.iter().take(node_count as usize) {
            let kind = node.object_type();
            let status = node.object_status();
            match kind {
                wct::WctObjectType::CriticalSection
                | wct::WctObjectType::SendMessage
                | wct::WctObjectType::Mutex
                | wct::WctObjectType::Alpc
                | wct::WctObjectType::Com
                | wct::WctObjectType::Unknown
                | wct::WctObjectType::SocketIo
                | wct::WctObjectType::SmbIo => {
                    let name = node.object_name();
                    let name = if name.is_empty() {
                        "?".to_string()
                    } else {
                        name
                    };
                    lines.push(format!("{:?} (status {:?}) \"{name}\"", kind, status));
                }
                wct::WctObjectType::ThreadWait
                | wct::WctObjectType::ProcessWait
                | wct::WctObjectType::Thread
                | wct::WctObjectType::ComActivation => {
                    lines.push(format!("{:?} (status {:?})", kind, status));
                }
            }
        }
        if is_cycle != 0 {
            lines.push("(cycle detected)".to_string());
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_tracker_fires_once_per_episode_and_rearms() {
        let threshold = Duration::from_millis(10);
        let mut tracker = StallTracker::new(threshold);
        // Under threshold: no fire.
        assert!(!tracker.observe(Duration::from_millis(5)));
        // First crossing: fires.
        assert!(tracker.observe(Duration::from_millis(10)));
        // Still stale: no refire while the episode continues.
        assert!(!tracker.observe(Duration::from_millis(99)));
        // Heartbeat landed (stall dropped): re-arms, but no fire yet.
        assert!(!tracker.observe(Duration::from_millis(1)));
        // Second episode: fires again.
        assert!(tracker.observe(Duration::from_millis(10)));
    }

    #[test]
    fn activity_ring_evicts_oldest_above_capacity() {
        let ring = ActivityRing::new(3);
        ring.push("a");
        ring.push("b");
        ring.push("c");
        ring.push("d");
        ring.push("e");
        assert_eq!(ring.snapshot(), vec!["c", "d", "e"]);
    }

    #[test]
    fn report_formatting_carries_the_evidence() {
        let data = ReportData {
            fired_ms: 1_784_000_000_000,
            stall_ms: 10_001,
            threshold_ms: 10_000,
            last_heartbeat_ms: 1234,
            pid: 4242,
            main_thread_rust_id: "ThreadId(1)".to_string(),
            main_thread_os_id: Some(77),
            episode: 2,
            cpu_line: "main thread cpu: 3.1% busy".to_string(),
            stack_lines: vec![
                "std::sys::backtrace::__rust_begin_short_backtrace+0x1a".to_string(),
                "mnemo_app::main::h1234+0x2b".to_string(),
            ],
            wait_chain_lines: vec!["CriticalSection (status Owned) \"foo::mutex\"".to_string()],
            activity: vec!["watchdog started".to_string(), "agent1 Started".to_string()],
        };
        let report = format_report(&data);
        assert!(report.contains("main thread stalled"), "report: {report}");
        assert!(report.contains("stall at fire: 10001 ms (threshold 10000 ms)"));
        assert!(report.contains("process id: 4242"));
        assert!(report.contains("main thread os id: 77"));
        assert!(report.contains("episode: 2"));
        assert!(report.contains("main thread cpu: 3.1% busy"));
        // Stack + wait-chain sections render frames and the unavailable
        // fallback respectively.
        assert!(report.contains("main thread stack at fire:"));
        assert!(report.contains("__rust_begin_short_backtrace+0x1a"));
        assert!(report.contains("main thread wait chain:"));
        assert!(report.contains("CriticalSection (status Owned) \"foo::mutex\""));
        assert!(report.contains("agent1 Started"));
        // Oldest-first ordering is preserved in the activity section.
        let started = report.find("watchdog started").unwrap();
        let agent = report.find("agent1 Started").unwrap();
        assert!(started < agent);
    }

    #[test]
    fn report_formatting_degrades_without_enrichment() {
        let data = ReportData {
            fired_ms: 1_784_000_000_000,
            stall_ms: 10_001,
            threshold_ms: 10_000,
            last_heartbeat_ms: 0,
            pid: 7,
            main_thread_rust_id: "ThreadId(1)".to_string(),
            main_thread_os_id: None,
            episode: 1,
            cpu_line: "main thread cpu: unavailable on this platform".to_string(),
            stack_lines: vec![],
            wait_chain_lines: vec![],
            activity: vec![],
        };
        let report = format_report(&data);
        assert!(report.contains("(unavailable on this platform)"));
        // Both unavailable sections appear.
        assert_eq!(report.matches("(unavailable on this platform)").count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn capture_thread_stack_walks_another_thread() {
        // Self-suspend would deadlock (SuspendThread defers a self-suspend
        // until the thread returns to the kernel), so walk a helper thread
        // in USER mode instead — the production shape (detector thread walks
        // the stalled main thread, which is blocked in a lock, not inside a
        // syscall). The `ready` flag guarantees the worker has left its
        // channel-send syscall before the capture: a thread suspended
        // mid-syscall is frozen in kernel mode and GetThreadContext returns
        // ERROR_PARTIAL_COPY (299) until resumed.
        let (tid_tx, tid_rx) = std::sync::mpsc::channel::<u32>();
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (ready2, stop2) = (ready.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            tid_tx
                .send(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
                .unwrap();
            ready2.store(true, std::sync::atomic::Ordering::Release);
            // Pure user-mode spin until the test releases us: always
            // context-readable while suspended.
            while !stop2.load(std::sync::atomic::Ordering::Acquire) {
                std::hint::spin_loop();
            }
        });
        let tid = tid_rx.recv().expect("worker must report its tid");
        for _ in 0..100 {
            if ready.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            ready.load(std::sync::atomic::Ordering::Acquire),
            "worker must reach the spin loop"
        );
        let frames = capture_thread_stack(tid);
        stop.store(true, std::sync::atomic::Ordering::Release);
        worker.join().unwrap();

        assert!(
            frames.iter().all(|f| !f.is_empty()),
            "frame lines must be non-empty: {frames:?}"
        );

        // DEPTH is the regression this guards. The old capture asked
        // dbghelp for symbols with fInvadeProcess = FALSE, so it started
        // with an empty module list: SymGetModuleBase64 returned 0,
        // SymFunctionTableAccess64 found no unwind data, and StackWalk64
        // stopped after ONE frame. Every hang report written before
        // 2026-08-24 carried 1-2 bare addresses and named no caller, which
        // is exactly the frame that identifies a deadlock. A spawned thread
        // has a real stack — closure, thread shim, BaseThreadInitThunk,
        // RtlUserThreadStart — so anything this shallow means the walk broke
        // again. `!frames.is_empty()` did NOT catch it; only depth does.
        assert!(
            frames.len() >= 5,
            "stack walk truncated to {} frame(s) — unwinding is broken again: {frames:?}",
            frames.len()
        );

        // ...and frames must be NAMED, not bare hex. System DLLs resolve
        // from their export tables even with no PDB present, so this holds
        // on any machine.
        assert!(
            frames.iter().any(|f| f.contains('!')),
            "expected module!symbol frames, got raw addresses: {frames:?}"
        );
        assert!(
            frames
                .iter()
                .any(|f| f.contains("RtlUserThreadStart") || f.contains("BaseThreadInitThunk")),
            "the walk should reach the thread entry point: {frames:?}"
        );
    }

    #[test]
    fn suspend_gap_rearms_without_a_heartbeat() {
        // A suspend/resume gap discards the measurement window rather than
        // satisfying it: the detector never sees a heartbeat, so `observe`
        // alone would leave the tracker disarmed and the NEXT genuine stall
        // would go unreported. `rearm` is what keeps the watchdog armed
        // across a machine sleep.
        let mut tracker = StallTracker::new(Duration::from_millis(10));
        assert!(
            tracker.observe(Duration::from_millis(50)),
            "first stall fires"
        );
        assert!(
            !tracker.observe(Duration::from_millis(50)),
            "still the same episode — no second report"
        );

        tracker.rearm();
        assert!(
            tracker.observe(Duration::from_millis(50)),
            "after a discarded window the next stall must fire again"
        );
    }

    #[cfg(windows)]
    #[test]
    fn capture_wait_chain_of_running_thread_is_graceful() {
        // The test thread is running, so its wait chain is short but valid.
        // The call must never panic and must return either lines or empty.
        let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
        let lines = capture_wait_chain(tid);
        for line in &lines {
            assert!(!line.is_empty());
        }
    }

    #[test]
    fn report_writer_creates_dir_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let data = ReportData {
            fired_ms: 42,
            stall_ms: 10_001,
            threshold_ms: 10_000,
            last_heartbeat_ms: 0,
            pid: 1,
            main_thread_rust_id: "ThreadId(1)".to_string(),
            main_thread_os_id: None,
            episode: 1,
            cpu_line: "main thread cpu: unavailable on this platform".to_string(),
            stack_lines: vec![],
            wait_chain_lines: vec![],
            activity: vec![],
        };
        let written = write_report(dir.path(), &data).unwrap();
        assert_eq!(written, dir.path().join("hang-42.txt"));
        let content = std::fs::read_to_string(&written).unwrap();
        assert!(content.contains("(none recorded)"));
    }
}
