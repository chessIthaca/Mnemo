# Diagnosis: 2026-08-20 app "crash" while researching the crash

**Plan:** 234a0219 (research) — "Diagnose the 2026-08-22 crash while researching the crash"
**Machine date at write time:** 2026-08-20 ~16:00 local (the machine clock has jumped — see E4)

## Verdict

The "crash" was **not a panic or exception — it was a Windows UI hang**.
`AppHangB1` / Event ID 1002: *"The program mnemo-app.exe stopped interacting
with Windows and was closed."* It happened **three times today** (11:35 AM,
3:29 PM, 3:44 PM), with an **identical hang signature (6978)** across
different builds — a reproducible, consistent blocking location, not random
flakiness. The exact blocked stack was not captured (WER recorded no wait
chain, and the app has no hang instrumentation), so the precise line is not
recoverable post-hoc — but the evidence ranks the likely mechanisms and
exonerates today's new features.

## Evidence timeline (2026-08-20, machine time)

| Time | Event | Evidence |
|---|---|---|
| 11:35 AM | Hang #1 | App log 1002/1001, P3 `6a8711da` (older build) |
| 12:00 PM | **Machine BSOD** — bugcheck 3b (SYSTEM_SERVICE_EXCEPTION), c0000005 | WER 1001 "BlueScreen" |
| 12:13:47 | Release build A | WER `TargetAppVer` of hang report 2 |
| 12:54:57 | Release build B (`target\release\mnemo-app.exe`) | WER `TargetAppVer` of hang report 3 |
| 14:04 | `0aa9783` shell-output-filter committed | git log |
| 14:52–14:53 | `636c5d4` live-turn-phases committed + merged (`3444c62`) | git log |
| 15:14 | `fc4a141` file_read/read_files invalid-args hint (HEAD) | git log |
| 3:22 PM | **ollama.exe memory-leak warning** (RADAR_PRE_LEAK_64) | WER 1001 |
| 3:29 PM | Hang #2 (build A) | App log 1002/1001, P3 `6a874e7b` |
| 3:44 PM | **Hang #3 (build B) — the "last crash while researching"** | App log 1002/1001, P3 `6a874e7b` |

Provider logs (`.coding/logs/`): last `traces.jsonl` entry is
`deepseek-v4-flash:0731` @ `ollama.com/v1` with `"stream aborted — consumer
dropped"` — the agent was mid-stream when the process died (hung → force
closed). Earlier, the glm-5.3 endpoint was returning **HTTP 429 "Weekly/
Monthly Limit Exhausted"** (4 consecutive 429s), which is why the session had
switched to ollama/deepseek. No provider error was logged at hang time — the
hangs were silent from the provider's perspective.

## Findings

### F1 — The app hangs on the UI thread; the "crash" is a hang (established)

Three `AppHangB1` events today, same signature `6978` / HangType `134217728`
across builds made hours apart. The process keeps running (the LLM stream was
still open when killed), but the window stops responding and Windows offers
"Close the program". This matches the app's historical failure mode (F1
blocking git poll, R1 main-thread `set_size`, R2 rAF event buffering) —
UI-thread/main-thread stalls are the recurring disease.

### F2 — Exact blocking location unrecoverable: instrumentation gap (established)

- Both WER hang reports contain **no wait-chain / thread-stack section**.
- `install_panic_hook` (`src/app/mod.rs:14`) only `eprintln!`s — and only
  fires on panics, which never happened.
- No app-side log files exist in `%LOCALAPPDATA%\com.mnemo.app` (only WebView2
  internal data).
- `.coding/logs/*.jsonl` only records provider requests; a hang mid-stream
  leaves just "consumer dropped".

→ **No evidence source captured the blocked stack.** The app needs a hang
watchdog (recommendation below) before this can be pinned to a line.

### F3 — Today's new features are exonerated (established)

The exe that hung at 3:44 PM was built at **12:54:57**; the shell-output-filter
(14:04) and live-turn-phases (14:52) commits landed *after* that build and are
not in it. The hang signature is also identical across the 12:13 and 12:54
builds. The historical fixes remain intact at HEAD: git branch poll runs via
`spawn_blocking` (`src-tauri/src/ipc/files.rs:594`), and the transcript is
capped via `capTranscript` (frontend store + `InputBar.tsx`).

### F4 — The machine environment is unstable (corroborating)

1. **BSOD at noon** (SYSTEM_SERVICE_EXCEPTION) — the box itself is unhealthy
   (VM/RDP host).
2. **ollama.exe memory leak** warning 7 minutes before hang #2 — the local
   model server degrading memory pressure mid-session.
3. **The system clock jumps**: review files named `2026-08-21-*` and
   `2026-08-22-*` were written today (8/20); WER filetime vs. local event time
   disagree by hours. Sessions have been dating themselves 1–2 days ahead.
   This is classic suspended-VM/RDP clock drift and explains the confusing
   dates in recent memories and report filenames.

## Ranked hypotheses for the hang location

1. **UI-thread stall under event flood during long agent sessions** (most
   likely). The hangs occur during research — continuous LLM streams plus
   bursts of tool/memory events into the frontend. The consistent signature
   suggests one specific blocking point (likely a sync call on the main/UI
   thread or a WebView2 render stall) reached only after the session has been
   busy for a while — consistent with "reasoning gets slower over time".
2. **Machine-level memory/CPU pressure** (contributing). ollama's leak +
   post-BSOD state degrade the box; pressure could turn a slow UI into a
   Windows-declared hang.
3. **A new main-thread blocking call** introduced in recent commits
   (cannot be confirmed or refuted without the stack — the watchdog will find
   it if it exists).

## Recommended next steps

1. **Add a hang watchdog** (implementation plan): a background thread that
   detects main-thread/UI stall >10 s, then writes a stack snapshot (walk the
   thread with `GetThreadWaitChain` + a backtrace) to
   `.coding/logs/hang-<timestamp>.txt`. This converts the next hang from
   silent to diagnosable.
2. **On the next hang**: collect the watchdog file + the new
   `AppHang_mnemo-app*` WER folder and compare signatures.
3. **Environment hygiene** (user actions): update/restart ollama (leak),
   investigate the BSOD dump (`C:\WINDOWS\Minidump`) with the VM host, and let
   the machine clock sync (or note it jumps — it is corrupting report/memory
   dates).
4. Optionally rebuild now: the merged shell-filter + live-turn-phases add UI
   updates (per-chunk token counters, phase transitions) — if hangs continue
   after a rebuild, the watchdog from step 1 will show whether the new
   frontend timers are implicated.
