## Verdict: PASS

# Re-review: perf/memory-cleanup-parallel (plan 0bcead88) after ae03bbc

Prior review: `.coding/reviews/2026-11-23-memory-cleanup-parallel-review.md` — FINDINGS (0 high, 2 low). Both fixed in commit `ae03bbc`. Working tree clean; reviewed committed sources `src/memory/maintenance.rs` and `src/memory/consolidation.rs`.

## Prior findings — verified fixed

### LOW 1 — error-path comment (maintenance.rs:166-174) — FIXED

The inline comment above `buffer_unordered` / `try_collect` now correctly states:
- `try_collect` stops on the first error **and CANCELS in-flight sibling sessions** (unlike the old sequential loop, which aborted before starting the next session).
- A cancelled sibling may be left with its episodic summary written but working rows intact.
- That is safe: each SQLite op is atomic; the next cleanup run re-consolidates that session (fresh episodic summary).
- Propagating errors are rare — consolidation swallows extract/delete failures; only list/write failures abort.

No overstatement of sequential equivalence remains. No code change was required; the comment alone was the finding.

### LOW 2 — SlowMockLlm sleep doc (maintenance.rs:434-439 vs :467) — FIXED

Struct doc now says "sleeps ~50ms"; `complete` sleeps `Duration::from_millis(50)`. Match.

## No new issues from the fix commit

`ae03bbc` is comment-only (the two doc corrections). No logic, lifetime, concurrency, or test changes. Nothing new to regress.

## Substance sanity re-check (unchanged from prior review; still clean)

- **Row claiming:** `.min()` over pending-filtered `&String` owners; `get_mut(owner.as_str())` into `HashMap<&str, _>` — correct first-owner-in-sorted-order claim.
- **Concurrency safety:** MemoryStore single write connection; `LlmClient: Send+Sync`; progress callback only from the driving task; completion-counted `AtomicUsize` ticks monotone.
- **Lifetimes:** `events: Vec<&Memory>` borrow `working`, which outlives the stream `.await`; `per_session.remove` moves each Vec into its future.
- **Test:** `cleanup_runs_sessions_concurrently` asserts `max_in_flight >= 2` (deterministic under `buffer_unordered` poll-pass overlap) and 12 calls for 4×3; end-state 4 episodic / empty working.
- **API split:** `consolidate_session` public signature preserved; `consolidate_session_with_events` is `pub(crate)`; agent / tool callers untouched.
- **Constitution:** public docs present; no `#[allow]`; no Windows-only APIs; external README/PLAN owe no update (cleanup engine not documented there).

## Findings

none
