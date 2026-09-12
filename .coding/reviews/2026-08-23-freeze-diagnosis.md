# Freeze diagnosis — 2026-08-23, 4:52 AM (first watchdog-evidenced hang)

**Plan:** 8332dacc (research sub-plan) — "Diagnose 2026-08-23 4:52 AM freeze from hang-watchdog evidence"
**Incident:** app froze mid-research session; user closed it via Windows "Close the program".

## Verdict

The main thread **blocked** (did not spin): the watchdog measured **0.0% CPU
over the 10 s stall window** (0 ms kernel+user over 10,004 ms wall) — an
uninterrupted wait on a lock or a native/COM call, not a busy loop. The stall
never recovered (episode 1, no re-arm) until Windows declared
**AppHangB1 (Event 1002) at 4:59:06 AM** and closed the app. Crucially, the
running binary (`target\debug\mnemo-app.exe`, built 8/23 3:17:23 AM) **postdates
the trace-freeze fix** (merge `1fc6ce8`, 8/22 7:11:58 PM) — this is a **new
blocking path**, not the fixed lock inversion on a stale build.

The exact line is still not recoverable: the watchdog (f4f1caf) records CPU% +
an activity ring but **no stack / wait-chain** — the remaining half of the
2026-08-20 instrumentation recommendation. Ranked locations below.

## Evidence timeline (8/23, machine time)

| Time | Event | Source |
|---|---|---|
| 3:17:23 AM | Running binary built (debug, post-fix) | exe LastWriteTime |
| ~3:42–4:29 AM | Normal agent activity; gemini 400, 4× "stream aborted — consumer dropped" (turn interruptions) | provider-errors.jsonl |
| ~4:52:24 AM | **Main thread blocks** (last heartbeat; 69.2 min after watchdog start) | hang-1787475154907.txt |
| 4:52:34 AM | Watchdog fires: 10,004 ms stall, **0.0% busy**, episode 1, pid 17180 | hang-1787475154907.txt |
| 4:59:06 AM | Windows AppHangB1 1002: "stopped interacting… closed" (repeat detections) | Application event log |
| 4:59:32 AM | In-flight LLM stream (deepseek-v4-flash, ollama.com/v1) aborts "consumer dropped" — the kill, not the cause | provider-errors.jsonl id:1 |
| 4:59:45 AM | Last log write; process exits | traces.jsonl mtime |

Activity ring's last notes: `agent1 Started` → `workflow → Executing` (no
`Finished`) — the agent was **mid-turn, LLM request in flight** when the main
thread stalled.

## The signature

All four watchdog-captured hangs (8/22 1:49–2:31 PM, pre-fix) plus this one
read **0.0% busy** — the recurring disease is a *blocked main thread*, and the
class survives the trace-lock fix. Single episode, no recovery, ~7 min to
Windows kill.

## Exonerated (checked, not the cause)

1. **trace.rs lock inversion** — fixed in the running build (see verdict).
2. **Sync tauri commands on the main thread** — none exist; every
   `#[tauri::command]` in `src-tauri/src/ipc/` is `async` (full scan).
3. **F1 git-branch poll** — fixed via `spawn_blocking` (2026-04-19).
4. **R1 event-loop `set_size`** — mitigated: agent-chat resizes run on a
   latest-wins worker (`main.rs:216-221`); browser-tab rects via `try_lock` +
   defer/heal (`browser_webview.rs:327-346`).
5. **Exit teardown** — ran at death (4:59), not at hang onset (4:52).
6. **Provider errors** — nothing at hang onset; the 4:59:32 abort is the
   consequence of the kill.

## Ranked hang locations

1. **WebView2 controller/compositor call executing on the main/UI thread and
   blocking on a wedged WebView2 browser process** (most likely). The app is a
   bare window with two child webviews (`main.rs:186-199`); every controller op
   (`set_size`/`set_position`/`show`/`hide`/`add_child`/`navigate`) is an STA
   call that transits or executes on the UI thread. The codebase itself
   documents that such calls "can block for a long time on a wedged compositor"
   (`browser_webview.rs:320-321`, `main.rs:206-209`, upstream
   WebView2Feedback #3581) — a failure mode already observed on this RDP box.
   Moving the *caller* to a worker thread protects the caller, not the main
   thread the call marshals through.
2. **Renderer wedge under event flood + a synchronous main-thread round-trip.**
   69 min into a session, the agent-chat webview absorbs continuous LLM/stream
   events; RDP occlusion throttling creates catch-up bursts
   (`main.rs:155-160`). A wedged renderer can pin the UI thread in paint/
   size/script round-trips at 0% CPU. Same disease family as F3/R2.
3. **Environment pressure as trigger** (contributing): unstable RDP VM — BSOD
   8/20, ollama.exe leak warning, clock jumps (2026-08-20 report F4).

## Recommended fixes (smallest first)

1. **Watchdog stack capture** (~100 LOC, closes the gap): on stall fire,
   `SuspendThread(main)` → `CaptureStackBackTrace`/`StackWalk64` →
   `ResumeThread`, plus `GetThreadWaitChain` to name the waited object; write
   frames + module offsets to the report (`cfg(windows)`, `windows-sys` already
   a dep; debug builds have pdbs). The **next** hang then names the exact line.
2. **Ring enrichment**: `watchdog_note` (events.rs:50) only records agent
   lifecycle — add notes for child-webview/controller ops (ensure/navigate/
   overlay/rect-apply) and browser-tool activity, so "was a controller call in
   flight?" is answerable from the report.
3. **User-side mitigation now**: set `MNEMO_DISABLE_WIN_OCCLUSION=1` (existing
   RDP escape hatch, `main.rs:155`) on this box; restart/patch ollama (leak).

Report: this file. Watchdog evidence: `.coding/logs/hang-1787475154907.txt`.
