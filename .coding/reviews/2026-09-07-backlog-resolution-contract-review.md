## Verdict: PASS

Zero findings (0 high, 0 low). Every claim in the new run_all.rs module-doc section, the README sentences, and the SPEC memory was verified against the actual code and is accurate; the three pin tests pin the load-bearing behavior with correctly-chosen find/rfind anchoring; the diff contains no logic edits (docs + tests + bookkeeping only); the queued done-orphan guard (6c6966b9) is correctly left unimplemented and is not blocked by the new pins.

### 1. Documentation accuracy — every claim verified against the code

**run_all.rs module doc, "Finished — the one notion (user ask 2027-01-07)" (:141-173):**

| Doc claim | Code | Verdict |
|---|---|---|
| Guarded transition table: terminal → `Pending` only | src/backlog.rs:476-490 — the terminal arm (:485-488) allows only `Done\|Failed\|CantResolve → Pending`; everything else `_ => false` | ✓ |
| `BacklogStore::requeue` accepts all three | src/backlog.rs:594-609 — `matches!(status, Failed \| CantResolve \| Done)`; refuses Pending/InFlight | ✓ |
| `clear_finished` soft-deletes exactly those | src/backlog.rs:696-713 — stamps `deleted_at` on Done/Failed/CantResolve only | ✓ |
| Backlog tab: retry re-queues any of the three; "Clear finished" gates on all three | BacklogView.tsx:602-605 (`canRetry`), :997-999 (`hasFinished`), :1055 (button title "done/failed/can't-resolve") | ✓ |
| Memory indexer scans live `Pending` only — terminal = history, plans indexed separately | src/memory/indexer.rs:593-634 — filter `i.status == "pending" && i.deleted_at.is_none()` (:615); the in-code comment states exactly this | ✓ |
| Run-all/auto-feed dispatch live `Pending` only — a terminal item is never re-run | `run_all_dispatch_next` → `next_pending_eligible` (src/backlog.rs:655-660, live + non-deferred); auto-feed → `next_pending` (:640-645, live) | ✓ (dispatch-selection claim — see the 6c6966b9 note below) |
| Clear finished NEVER clears a mid-flight item | clear_finished touches only the three terminal statuses; Pending+InFlight kept — pinned by `clear_finished_drops_terminal_statuses` (src/backlog.rs:1625-1646, asserts remaining = [pending, inflight]) | ✓ |
| `deleted_at` stamp; JSONL line + image sidecars stay; 30-day startup purge; union-merge-safe | clear_finished doc comment, src/backlog.rs:690-695 | ✓ |
| Run's done/total counters are run-scoped in-memory state — mid-run clearing does not (and cannot) touch them | `RunAllState` (src-tauri/src/ipc/state.rs:214-223): AtomicBool/Mutex/AtomicU64, no persistence; constructed fresh per run (backlog_cmds.rs:467-472); `backlog_clear_finished` body (:222-229) locks only the store + emits the event | ✓ |
| Adoption sweep snapshots `store.items()` — the live-only view — so a soft-deleted InFlight item can never be resurrected | `adopt_orphaned_in_flight` (run_all.rs:2435-2471) reads `store.items()` (:2453-2454) and filters `BacklogStatus::InFlight` (:2457); `items()` filters `is_live` (src/backlog.rs:721-723) | ✓ |
| **Escape hatch**: agent-stamped terminal wins over the automatic `Done` stamp; the guarded table refuses the later terminal→terminal transition; the run still counts it resolved (done counter bumps, whichever terminal status) | `on_main_turn_resolved`: the gate arm's `transition(&item.id, Done, None)` (run_all.rs:2015-2019) is a no-op refusal when the item was pre-stamped terminal (table :476-490; pinned by `transition_refuses_every_illegal_row`, src/backlog.rs:1441-1444 — Done→Done, Done→Failed, Failed→CantResolve, Cant→Done all refused); control then falls through to the **unconditional** done-counter bump (:2093-2096) and in-flight-pointer clear (:2105) | ✓ — the review-focus concern is confirmed accurate |

**README sentences (the one extended Backlog-tab line):** each new clause maps to verified code — retry re-queues any of the three (requeue + canRetry); Clear finished soft-deletes exactly those three and never a mid-flight item; Run-All dispatches pending only with run-scoped counters mid-run clearing cannot touch; main-agent exit / hard crash requeues the orphaned in-flight item to Pending via the exit drain (`drain_run_all_on_main_exit`, run_all.rs:2370-2402 — transition to Pending, checkpoint sha preserved in the note) and the run-start adoption sweep; the agent-stamped terminal status wins over the later automatic stamp. No over-claims found.

**"A terminal item is never re-run" vs the queued done-orphan guard (6c6966b9):** NOT an over-claim. The doc's claim is about dispatch *selection* (every dispatch path selects live Pending only — true). The 6c6966b9 incident item (35b94671) was **InFlight**, not terminal: the session died between committing and finishing, so the item never received a terminal stamp, and the start drain then legitimately requeued the InFlight orphan per the documented contract. Work-landed evidence detection is a different notion from terminal-ness and remains correctly queued as a separate task.

### 2. Pin tests — load-bearing behavior, correctly anchored patterns

- **`adopt_orphaned_in_flight_snapshots_the_live_only_view`** (run_all.rs): `rfind` is correct — the needle `pub(crate) async fn adopt_orphaned_in_flight` occurs exactly twice in the file: :413 (this test's own literal — the tests module precedes the functions here) and :2435 (the real definition); `rfind` anchors the definition. The sliced body (:2435-2471, first `\n}\n` = the function's own column-0 closing brace) contains both pinned strings: `.items()` (:2454) and `BacklogStatus::InFlight` (:2457). The pinned behavior is exactly the load-bearing one — the live-only snapshot that makes soft-deleted InFlight invisible to adoption.
- **`backlog_clear_finished_touches_only_the_store_and_the_event`** (backlog_cmds.rs): `find` is correct — the needle occurs at :222 (the command; the tests module starts at :495, *after* the command) and :737 (the test's own literal); `find` anchors the command. The body (:222-229) contains `clear_finished()` (:226) and `emit_backlog_changed(&app, &state)` (:227) and no `run_all` substring — the isolation claim (mid-run clearing cannot corrupt the run's counters/pointer) holds. Mirrors the established `backlog_retry_routes_through_guarded_requeue` source-contract pattern (:509-529).
- **hasFinished pin** (BacklogView.test.ts): asserts the exact combined gating expression `'b.status === "done" || b.status === "failed" || b.status === "cant_resolve"'`, matching BacklogView.tsx:998 verbatim. This is *stronger* than the existing canRetry pin's three separate `toContain` calls (those would pass even if a status string appeared in an unrelated condition) — it pins the actual combined condition.

### 3. No behavior changes

The full diff was read end-to-end: run_all.rs +65 lines (34 module-doc `//!` comment lines + a 31-line test inside `mod tests`), backlog_cmds.rs +33 lines (test only, inside `mod tests` at :495+), BacklogView.test.ts +14 lines (test only), README.md one line extended, .coding/backlog.jsonl bookkeeping (item 998f85fc pending → in_flight with plan_id), plus two untracked bookkeeping files (the SPEC memory + the plan file). Zero logic edits anywhere — the deliverable is documentation + pins, exactly as the plan promised.

### 4. Documentation sync

- README.md: the Backlog-tab paragraph now carries the user-facing contract — verified accurate (section 1).
- Module doc: the new section sits after the existing resolution arms (:103-139) and is consistent with them (same notions of terminal/requeue/adoption; no contradictions).
- SPEC memory (.coding/knowledge/spec/2027-01-07-backlog-resolution-contract-finished-terminal-cl.md): consistent with the module doc and the code, including the scope-boundary note.
- PLAN.md / endpoints.toml: not applicable (no provider/config change).

### 5. Multi-platform neutrality

Docs + tests only. No platform-specific APIs, paths, or shell syntax anywhere in the diff.

### 6. Security / regressions

None. No new inputs, no logic changes, no attack surface. The source-contract tests read their own source via `include_str!` / `?raw` imports — no runtime effect.

### 7. Scope boundary respected

The done-orphan guard (backlog 6c6966b9) is NOT implemented: no changes to `drain_run_all_on_main_exit`, to `adopt_orphaned_in_flight`'s logic, or to dispatch. The new adoption pin asserts the *current* contract (`items()` + InFlight filter); the queued guard's fix direction (work-landed evidence check before requeueing) keeps both pinned strings, so the pin does not block the future task.

### Minor observations (no action required — not findings)

1. The run_all.rs test's doc comment says "The sweep needs a Tauri AppHandle" while the function signature is `adopt_orphaned_in_flight(state: &IpcState)` — defensible shorthand (IpcState is only obtainable via the Tauri app in production, so a unit test is impractical either way); the sibling backlog_cmds.rs test's phrasing ("needs Tauri state") is the precise one.
2. Pre-existing, out of scope, and not claimed either way by the new docs: a mid-run **retry** (not clear-finished) of a failed item re-queues it to Pending, and the run can later dispatch it again and bump `done` past the start-time `total` (e.g. 3/2) — the counters are run-scoped by design. Noted only so the next contract touch knows.
3. Test status at review time (all green: cargo lib 2079+16; src-tauri 245+4+2; frontend tsc exit 0 + vitest 75 files / 1045 tests) is consistent with this review's source-level verification — every assertion in the three new tests (needle, anchor, and sliced body) was checked against the actual file contents and matches.
