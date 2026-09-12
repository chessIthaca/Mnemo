## Verdict: FINDINGS (0 high, 2 low)

# Review: perf/memory-cleanup-parallel (plan 0bcead88)

Reviewed the full uncommitted diff (`git diff HEAD`): `src/memory/consolidation.rs` (+23/-~4), `src/memory/maintenance.rs` (+~200/-24), plus `.coding/plans/stack.json` (bookkeeping). Untracked plan doc `.coding/plans/0bcead88-….md` is bookkeeping, not reviewed as code.

## What was verified (clean)

**consolidation.rs split**
- `consolidate_session` keeps its public signature and is a thin wrapper: `list_by_tier(Working)` → filter by session id → delegate to `consolidate_session_with_events` (consolidation.rs:107-121). The extracted `pub(crate)` core (:133-195) is byte-for-byte the former body: empty events → `Ok("")`; synthesize (LLM with synthetic fallback); write episodic; extract semantic + procedural (errors swallowed via `let _`); `delete_working_for_session`; `Ok(id)`.
- Other callers untouched, confirmed by `git status` and direct read: `src/runtime/agent.rs:623` (`spawn_consolidation`) and `src/tool/memory/mod.rs:443` (`memory_consolidate`) — neither file is modified.

**Row claiming correctness** (maintenance.rs:146-160)
- `sessions` comes from a `BTreeSet<String>` (lexicographic ascending); `.min()` over the pending-filtered `source_session_ids` iterator of `&String` returns the lexicographically smallest pending owner — the same "first owning session in sorted order" the sequential loop effectively used. Traced all cases: (a) row tagged with one pending session → that session (same as before); (b) row tagged with multiple pending sessions {A<B} → claimed by A, summarized under A, deleted by A's `delete_working_for_session` — identical to the old loop where A ran first and its filter grabbed the shared row; (c) session whose every row is claimed by an earlier session → empty event list → `Ok("")`, not counted — identical to the old loop re-listing and finding nothing. Deletion is by session-id tag (mod.rs:1669-1690, JSON1 `json_each` containment) and is idempotent, so overlapping delete sets cannot double-delete or corrupt; `working_rows_removed` is pre-counted on the snapshot, unchanged.
- `per_session.get_mut(owner.as_str())`: `HashMap<&str, _>` lookup by `&str` via `Borrow<str>` — valid; `owner ∈ pending` and `pending` is exactly the map's key set, so `expect("owner in map")` is unreachable.

**Concurrency safety**
- `buffer_unordered(CLEANUP_CONCURRENCY)` polls the buffered futures inside the single task awaiting the stream — overlap comes from interleaving at `.await` points (the LLM calls), which is precisely the intended win (~12 in-flight LLM calls). SQLite writes cannot race: all writes go through `spawn_blocking` + the single `Arc<Mutex<Connection>>` write connection (mod.rs:323-336; delete at :1670-1673 takes the lock). `LlmClient: Send + Sync` verified (provider/mod.rs:341); `provider`/`corpus` are shared immutably. The `progress` callback is only ever invoked from the one driving task (never concurrently with itself), and `completed.fetch_add(1)+1` keeps `done` monotone 1..=total; the IPC consumer (src-tauri/src/ipc/memory_maintenance.rs:279-289) only emits `done/total`, so completion-order ticks are fine, and the doc comment now states the order is nondeterministic.
- Lifetimes: `events: Vec<&Memory>` borrow `working`, which outlives the in-function `.await` of the stream; `per_session.remove()` moves each Vec into its future. No aliasing bug.

**Error/abort semantics** — `try_collect` stops on the first `Err` and propagates it, preserving abort-on-error. One nuance, Finding 1 below.

**Test quality** — `cleanup_runs_sessions_concurrently` (maintenance.rs:480-536) is meaningful and NOT flaky:
- If the code were sequential, `max_in_flight` would be exactly 1 (each session awaited to completion before the next starts) → assertion fails. The test exercises the changed path and discriminates the regression.
- With `buffer_unordered(4)`: the driver polls session 1 → its `complete` bumps `in_flight` to 1 and pends on the 50ms sleep; the driver immediately polls session 2 → `in_flight` = 2. This happens synchronously within one poll pass, so `max_in_flight >= 2` is deterministic (typically reaches 4), not timing-dependent.
- The 12-call assertion pins exactly 3 LLM calls per session (synthesize + semantic + procedural); the mock's object-JSON degrades semantic/procedural extraction to empty arrays safely, so the calls happen without writes. End-state assertions (4 episodic, empty working tier, report {4, 4}) round it out. `multi_thread, worker_threads = 4` is fine (not strictly required for overlap, but closer to production).

**Constitution compliance**
- Doc comments on all public items; the new `pub(crate)` fn, `CLEANUP_CONCURRENCY`, the `cleanup` fn doc (bounded concurrency + new progress semantics), and the module doc are all updated. No `#[allow(...)]` added. No Windows-only APIs / `cfg(windows)` — `futures` is cross-platform.
- Documentation sync: `README.md` and `PLAN.md` contain no references to the cleanup engine (grepped), so no external doc updates are owed; the behavior change is fully covered by the in-code docs.
- `#![deny(warnings)]`: the reviewer is read-only and could not re-run `cargo test`; static inspection shows no unused imports (`StreamExt`→`buffer_unordered`, `TryStreamExt`→`try_collect`, `Memory`, all test imports used) and no unused bindings. Green-build claim rests on the main agent's test run.

## Findings

### LOW 1 — Error path: `try_collect` cancels in-flight sessions mid-pipeline; inline comment overstates equivalence (maintenance.rs:166-168, 189-191)

The comment says `try_collect` gives "the same abort semantics as the former sequential loop." Not quite: the old loop aborted *before starting* the next session — a running session always finished its pipeline. With `buffer_unordered` + `try_collect`, when one session errors the stream is dropped, **cancelling up to `CLEANUP_CONCURRENCY - 1` sibling sessions mid-flight**. A sibling cancelled between `store.write(episodic)` (consolidation.rs:174) and `delete_working_for_session` (:192) is left with its episodic summary written but its working rows intact; the next cleanup run re-consolidates that session and writes a **duplicate episodic summary** for it. No corruption — each SQLite op is atomic and serialized, and consolidation swallows extract/delete errors so a propagating error is rare (list/write failures only) — but it is a real behavioral difference on the error path. Suggested fix: correct the inline comment (and optionally the `cleanup` doc) to say in-flight sessions are cancelled on error and partial sessions are safely re-consolidated on the next run; no code change required.

### LOW 2 — Stale doc comment on `SlowMockLlm`: says "~60ms", code sleeps 50ms (maintenance.rs:428-433 vs :461)

The struct doc says "sleeps ~60ms (long enough for the cleanup concurrency to overlap several calls…)" but `complete` sleeps `Duration::from_millis(50)`. Fix the comment to 50ms.

## Non-finding observations (no action required)

- `let session_events = events;` (consolidation.rs:143) is a no-op rebinding kept to minimize the diff (uses below take `&session_events`, now `&&[&Memory]`, which deref-coerces). Harmless; could be dropped in a future cleanup.
- On error, the returned error is the first in *completion* order rather than sorted-session order (old loop). Cosmetic only.
- `expect("owner in map")` (maintenance.rs:158) is provably unreachable (owner ∈ pending = key set) — acceptable as written.
