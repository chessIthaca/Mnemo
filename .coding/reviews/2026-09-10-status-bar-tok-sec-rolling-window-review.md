## Verdict: FINDINGS (0 high, 3 low)

The rolling last-3-requests window is correctly implemented end-to-end and the scope guard held exactly — only the InflightBar display changed; StatsView, the per-request `lastRequestTiming` rates, traceStats.ts, and the sessionTiming totals are untouched. All three findings are comment/copy precision issues (no correctness, security, or platform problems). Fix them and this is ready.

### What was reviewed
All uncommitted changes on wt/agenticcoding vs HEAD (8 files, +171/−31): the six frontend source/test files of plan 70328e41, plus `.coding` side-car state (backlog.jsonl, a knowledge-spec amendment, the untracked plan file).

### Correctness — verified
1. **Window append (agentEventReducer.ts:794-800):** `gen !== null && gen > 0` gate, spread + `.slice(-RECENT_TIMING_WINDOW)`, newest last, immutable (reuses the old array reference when gen is absent). Correct ring buffer; the gate excludes samples with no time denominator.
2. **recentOutputTokPerSec (agentState.ts:149-161):** total-over-total via `aggregateTokPerSec` (sum tokens / sum ms) — not average-of-rates. Empty window → 0/0 → `null` (msTotal = 0); a non-empty window always has ms > 0 (every entry passed the gen > 0 gate), so there is no null-rate-with-data edge.
3. **All three SessionTiming constructors updated** — `emptyAgentState` (agentState.ts:383-390), `reduceUsage` (agentEventReducer.ts:801-809), `clearConversation` (useAgentStore.ts:978-985). The InputBar clear path (InputBar.tsx:447-452) resets through `emptyAgentState()` — covered. No AgentState persistence (only geometry/theme/panel widths hit localStorage) → no stale-shape rehydration risk.
4. **Scope guard held:**
   - StatsView.tsx untouched — still imports `aggregateTokPerSec` from useAgentStore with its 7 call sites (ModelTable per-model + totals, SessionCard, ProjectCard) over the all-requests totals.
   - `lastRequestTiming` per-request rates untouched (agentEventReducer.ts:781-784, 820-825).
   - traceStats.ts not in the diff.
   - sessionTiming totals still accumulate over ALL requests (agentEventReducer.ts:801-808) — only `recentTiming` was added.
   - InflightBar's import swap is complete (`aggregateTokPerSec` no longer imported, only mentioned in a comment); useAgentStore still re-exports it for StatsView + tests.
5. **Display (InflightBar.tsx:316-341):** '—' placeholder when the rate is null (per plan — previously the span hid); outer gate `timed_requests > 0` preserved; `avgTtftMs`/`avgGenMs` still used by the outer tooltip (:323) — no dead code (clean tsc corroborates).
6. **Tests:** the new reducer test pins the 4-request rollover (oldest dropped), window order, total-over-total math (300 tok / 11 s), and the gen-null exclusion (a 999-token request doesn't enter the window); agentState.test.ts pins empty→null and the average-of-rates trap (30 vs 50 — arithmetic verified correct there). The two toEqual shape updates are mechanical and match their events (50/2000). Existing aggregateTokPerSec tests unchanged.
7. **Security / platform / file-tools:** pure display math — no new input, injection, or platform surface; no shell-based file mutation anywhere in the diff.

### Documentation sync — checked, no gaps
- README.md: the inflight-bar bullet (:74) makes no session-wide tok/sec claim; "session aggregates (totals, tok/s…)" at :76 is the Trace tab (untouched). No update needed.
- PLAN.md: the only tok/s mention (:1052, per-endpoint Stats rows) is untouched and still true.
- Module doc comments: SessionTiming, recentOutputTokPerSec, RECENT_TIMING_WINDOW, reduceUsage, and the InflightBar comments are updated and accurate (except LOW 2 below).
- Knowledge files: no living spec documents the status-bar rate as session-wide; the tok/s specs (per-endpoint breakdown, aggregateTokPerSec) remain accurate.

### Findings

**LOW 1 — wrong counterfactual arithmetic in the new test's comment (useAgentStore.test.ts ~:1018-1020).** The comment says the average of the per-request rates "would be (100 + 33.3 + 50) / 3 ≈ 61.1" — but 100 tok/s is the DROPPED first request (the 1000 ms sample) and the window's 16.7 tok/s member (the 6000 ms sample) is omitted. The window's rates are 33.3 / 16.7 / 50 → average ≈ 33.3 tok/s. The assertion itself (`toBeCloseTo(300 / 11)`) is correct; only the comment misleads a future reader about the trap's magnitude. Fix: "(33.3 + 16.7 + 50) / 3 ≈ 33.3".

**LOW 2 — imprecise comment gloss (InflightBar.tsx:319).** "'—' while the window is still empty (no timed requests yet)" — the window only holds generation-timed requests, so '—' can render while `timed_requests > 0` (a ttft-only usage event increments the counter but never enters the window; the segment renders because of the outer gate). Reword to "no generation-timed requests yet" so the comment matches the gen > 0 gate the reducer documents.

**LOW 3 — tooltip hardcodes the window size (InflightBar.tsx:329).** "Output tok/sec over the last 3 requests…" duplicates RECENT_TIMING_WINDOW as a magic number in user-facing copy; tuning the const would silently stale the tooltip. Interpolate: `` title={`Output tok/sec over the last ${RECENT_TIMING_WINDOW} requests (completion / generation time)`} `` (import/re-export the const alongside recentOutputTokPerSec).

### Side-car state riding the commit (observations, not findings)
- `.coding/backlog.jsonl`: this plan's item a21993a4 pending→in_flight with plan_id 70328e41 — expected dispatch bookkeeping. It also carries an UNRELATED soft-delete (`deleted_at`) of item f3982dae "Cache-affinity endpoint routing" — user/app activity outside this plan, benign under the union merge driver (the documented soft-delete pattern; prior reviews accepted the same). It will ride this commit — confirm that's intended before committing.
- `.coding/knowledge/spec/2027-01-07-plan-resumability-gate-…md`: +2-line merged-into-main amendment from the previous plan's merge — expected bookkeeping.
- Untracked `.coding/plans/70328e41.md` — this plan's file; include it in the commit.

### Verification note
Read-only reviewer — I could not re-run `npm test` / `tsc` myself; the reported 1073 tests green (77 files) + clean tsc is consistent with my inspection (all SessionTiming shapes consistent across constructors and tests, no dangling references, no unused imports).
