# Memory cleanup "stuck at 100%" — live diagnosis (2026-08-18 ~11:43–11:58)

## Verdict

**The cleanup IS still running and progressing.** It is not wedged. It is just
extremely slow with a provider configured: ~101 stale sessions × 3 sequential
LLM calls per session (synthesis + semantic extraction + procedural
extraction) at the configured reasoning effort (app default `max`).

## Live evidence (read-only probes)

memory.db is `.coding/memory.db` (77.9 MB; WAL 78.4 MB — autocheckpoint
lagging under write pressure; a checkpoint ran at 11:02:01).

Three read-only snapshots via `python .coding\analysis\memcheck.py`:

| time     | working | episodic | pending sessions | event                          |
|----------|---------|----------|------------------|--------------------------------|
| 11:53:13 | 6598    | 102      | 101              |                                |
| 11:54:38 | 6574    | 102      | 100              | session K's working rows swept |
| 11:58:05 | 6574    | 103      | 100              | session K+1's summary written  |

=> one session consolidates every ~4 minutes; ~100 sessions remain
=> ETA on the order of **hours** (100 × ~4 min ≈ 7 h).

App process `myharness-app` (pid 52312) CPU delta ≈ 0.5% over 85 s —
waiting on network LLM calls, not local compute.

`provider-errors.jsonl` last error 09:53:49 (before the run) => the
cleanup's LLM calls are succeeding (consolidation swallows LLM errors and
degrades to synthetic; failures would be logged there).

## Why the bar reads "100%"

- Progress ticks fire only AFTER a session fully consolidates
  (maintenance.rs: `progress(Consolidating, i+1, total)`), so the bar jumps
  ~1% every few minutes.
- With total ≈ 101, `Math.round(done/total*100)` can never render "100"
  mid-run (max 99%). What the user saw is the **indeterminate full-width
  pulsing bar** (done=0, total=0 → `w-full animate-pulse`): shown after
  `started` until the FIRST tick — i.e. for the entire first session's
  ~4–14 min of LLM calls — caption "working…". A full-width bar reads as
  "stuck at 100%".
- After the first tick it shows "1%" and crawls ~1%/session.

## User-side live checks (no shell needed)

1. **Trace tab** (right panel): cleanup's LLM requests appear live
   (single-user-message prompts: "Summarize this coding session as JSON…",
   "Extract distinct, reusable facts…", "Identify any recurring workflow…").
   An in-flight request = actively consolidating.
2. **Memory debug tab**: polls every 2 s — watch `working` fall and
   `episodic` rise.
3. **Busy-guard probe**: click "Clean up now" again — "a memory maintenance
   operation is already running" proves the backend op is alive.

## True hang windows found (not the cause here, but real)

1. `OpenAiClient::complete().await` waiting for **response headers** is
   unbounded: `connect_timeout` (30 s) covers only the handshake, the 90 s
   READ_TIMEOUT applies only per-chunk AFTER headers. A server that accepts
   and never replies wedges the op forever (no terminal event, busy flag
   never clears; only an app restart recovers). The stream phase itself is
   bounded (90 s/chunk, errors swallowed → synthetic fallback → run
   continues).
2. VACUUM is bounded (`busy_timeout=5000` → SQLITE_BUSY → Failed event);
   a 78 MB DB vacuums in seconds. While it runs, agent memory writes
   serialize behind the single-writer mutex (brief).

## Fix options (follow-up implementation plan)

- Emit a per-session START tick (e.g. `(Consolidating, i, total)` before
  processing session i, or a "session i/N" label) so the UI never sits on
  the misleading full-width pulse during the longest silent window.
- Frontend: never display 100% while running (cap at 99%); render the
  indeterminate state visually distinct from a full bar.
- Wrap each consolidation LLM call in a total timeout (e.g. 5 min) to close
  the wait-for-headers hang window.
- Consider lower reasoning effort / cheaper model for maintenance
  consolidation, or a sessions-per-run cap. (Design decision for the user.)
- Optional: a Cancel button (abort the engine task; BusyGuard drops).
- Restarting the app is safe mid-run: per-session consolidation is
  independent + committed incrementally; already-swept sessions stay swept;
  a future run resumes the remainder.
