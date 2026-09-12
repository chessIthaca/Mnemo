# Trace File Logging ("Log to file" checkbox → `.coding/logs/traces.jsonl`) — Review

**Date:** 2026-08-13
**Reviewer:** read-only reviewer subagent
**Scope:** ALL uncommitted working-tree changes (`git diff HEAD` + untracked): `src/provider/trace.rs`, `src-tauri/src/main.rs`, `src-tauri/src/ipc/trace.rs`, `frontend/src/lib/tauri.ts`, `frontend/src/components/views/LlmTraceView.tsx`, `.gitignore`, `.coding/backlog.json`, `.coding/plans/*` (bookkeeping).

## Verdict

The feature is functionally correct and matches the plan: path set at startup (`src-tauri/src/main.rs:356-360`), logging opt-in via checkbox, records mirrored as single-line JSON upserted by `id`, `.coding/logs/` gitignored, api_key confirmed absent from the log (it only ever goes into the `Authorization` header at `src/provider/openai.rs:394`; `build_request_json` at `openai.rs:656` builds only the POST body). No deadlocks (see below). Findings are all minor/perf/doc-level — nothing blocks commit, but items 1–3 should be considered (1 and 3 are cheap doc fixes; 2 is a design tradeoff worth a comment).

## Findings

### Correctness

**1. (minor) `start()` does not mirror the record to the file — the first write happens on the first mutation.** `LlmRequestLog::start` (`src/provider/trace.rs:180-212`) pushes the record into the ring but never calls `write_record_to_file`; only `with_record` (`trace.rs:298-306`) does. In practice every request is always followed by at least one `with_record` mutation — `set_status` on success (`openai.rs:420-422`) or `fail` on transport/HTTP error (`openai.rs:402-405`, `413-415`) — so every record does land in the file. But a crash in the window between `start()` and the first response loses the request line entirely, and a hypothetical future caller that starts a record and never mutates it would be silently absent from the log. Suggested fix (cheap): either mirror in `start()` too when enabled, or add one doc sentence on `start()` noting "the record is first mirrored on its first update".

**2. (perf note, not correctness) Whole-file rewrite per streamed chunk, unbounded file.** `write_record_to_file` (`trace.rs:345-384`) does a full `read_to_string` + parse-every-line + `fs::write` on **every** SSE chunk (`append_response` funnels through `with_record`). The ring is capped at 32 records (`MAX_RECORDS`), but the file is *not* — it is append/upsert-forever, so over a long enabled session it grows without bound and each chunk rewrite is O(total file size, including full request bodies with tool outputs). A long streaming response over a multi-MB log means hundreds of MB of rewritten disk I/O for one request. This matches the documented "debugging aid" tradeoff, but two things deserve at least a comment: (a) the file has no rotation/cap counterpart to `MAX_RECORDS` — say so in the `write_record_to_file` doc; (b) `std::fs::write` (`trace.rs:383`) is not atomic — a crash mid-write can leave a torn file that loses *all* previously logged lines, not just the in-flight one (a write-to-temp-then-rename would make it crash-safe; acceptable to skip for a debug aid, but the doc should state the durability caveat).

**3. (minor, docs) Module doc now slightly stale.** `trace.rs:17-18` still says the log "is deliberately in-process + session-only — cache-hit debugging is a live-inspection workflow, not a persistent audit trail." With this change there is now an opt-in persistent mirror. Update the module doc to mention the opt-in file mirror so the header doesn't contradict the feature.

### Lock ordering / concurrency (checked — no finding)

- `with_record` holds `records` (Mutex) then takes `log_path` inside `write_record_to_file` (`trace.rs:299→347`). `set_log_file_path` (`trace.rs:312`) and `set_logging_enabled` (`trace.rs:319`) take **only** `log_path`, never `records`. No path acquires `log_path` then `records`, so there is no lock-cycle → no deadlock. Ordering is consistent (records → log_path).
- Enable/disable race: `log_enabled` is `AtomicBool` with `Relaxed` load/store (`trace.rs:302, 327, 332`). A `with_record` that already loaded `true` before a concurrent `set_logging_enabled(false)` will still complete its one write — that write holds the `records` lock, so it can't corrupt the file; subsequent mutations see `false`. This is the correct "stops on next update" semantics the test asserts (`trace.rs:595-598`). The AtomicBool is the single source of truth the IPC reads (`ipc/trace.rs:40-41`). OK.
- Tauri commands are sync fns (`ipc/trace.rs:40, 47`) → they run on Tauri's blocking thread pool, not the async runtime; the brief `log_path` Mutex hold + `create_dir_all` cannot stall request streaming beyond one records-lock hold. OK.

### Checkbox wiring (checked — no finding)

- Mount fetch: `useEffect(..., [])` with a `cancelled` flag calls `getTraceLogging()` and sets `logToFile` (`LlmTraceView.tsx:567-579`). The view unmounts on tab switch and remounts on return, so the checkbox re-syncs from the backend each time — correct.
- Optimistic toggle + revert-on-error: `toggleLogToFile` sets state first, awaits `setTraceLogging`, reverts + surfaces the error banner on failure (`LlmTraceView.tsx:696-705`). Matches the plan. Minor UX nit (not a finding): two rapid toggles could land out of order; self-heals on next mount.
- Tooltip text matches plan step 5. Checkbox sits next to Clear in the header (`LlmTraceView.tsx:721-741`). OK.

### Constitution compliance

- All new public Rust fns have doc comments: `set_log_file_path` (`trace.rs:308-311`), `set_logging_enabled` (`317-318`), `logging_enabled` (`330`) — and both IPC commands (`ipc/trace.rs:36-38, 44-45`). Frontend wrappers have JSDoc (`tauri.ts:776-784`). PASS.
- No new `#[allow(...)]` in this diff (the two `#[allow(clippy::too_many_arguments)]` in `src/agent/loop_impl.rs:174,211` are pre-existing, untouched). No `@ts-ignore`/`@ts-expect-error`/new `eslint-disable` in changed frontend files. PASS.
- `.gitignore` gains `.coding/logs/` alongside `.coding/memory.db*` (`.gitignore:47`) — traces can't be committed. PASS. (Note: `.coding/backlog.json` and `.coding/plans/*` changes are plan bookkeeping and are intentionally committed.)
- `tempfile` (used by the new tests) is already a dev-dependency in `Cargo.toml:60`. PASS.
- Warning-free build and green `cargo test`/`npm test`/`npm run build` are the main agent's closing-sequence responsibility — I did not run builds (read-only).

### Bugs / security

- **api_key never reaches the log.** The key is sent only as the `Authorization: Bearer` header (`openai.rs:394`); the traced `request_json` is the POST body built by `build_request_json` (`openai.rs:656`), which contains model/messages/tools only. PASS.
- Path is project-sandboxed: `project.coding_dir.join("logs").join("traces.jsonl")` (`main.rs:360`); `set_logging_enabled(true)` creates the parent dir (`trace.rs:320-325`). PASS.
- Silent write-failure swallowing (`let _ = std::fs::write`, `trace.rs:383`) is acceptable for an opt-in debugging aid and is consistent with the doc. No finding.
- The log file contains full request bodies (messages + tool outputs) by design; it's gitignored and off by default. Users should be aware enabling it persists potentially sensitive conversation content to disk in plaintext — the tooltip says *what* it writes but not that content may be sensitive. Optional wording tweak, not a finding.

## Suggested actions (all minor)

1. Mirror in `start()` too, or document "first mirrored on first update" on `start()` (finding 1).
2. Extend the `write_record_to_file` doc with: file is unbounded (no `MAX_RECORDS` counterpart) and `fs::write` is non-atomic (crash can tear the whole file) (finding 2).
3. Update the module doc (`trace.rs:17-18`) to mention the opt-in persistent mirror (finding 3).

No blocking findings.
