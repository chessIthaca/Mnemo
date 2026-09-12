# Review: Fix compaction bugs (fix/compact-bugs)

Scope: all uncommitted changes on `fix/compact-bugs` (git diff HEAD): `src/agent/context.rs`, `src/agent/turn.rs`, `src/runtime/agent.rs`, `src/runtime/channels.rs`, `frontend/src/lib/types.ts`, `frontend/src/hooks/agentEventReducer.ts`, `frontend/src/components/layout/InputBar.tsx`, `frontend/src/lib/slash.ts`, `frontend/src/hooks/useAgentStore.test.ts`, `frontend/src/lib/ipc-contract.test.ts`, `frontend/src/lib/ipc-fixtures/event-compact-started.json` (new), `README.md` (+ `.coding/*` bookkeeping).

## Findings

### 1. MEDIUM — Failed mid-turn compaction resumes with a false harness note (behavior change on the error path)

`src/runtime/agent.rs` — `drive_turns` Compact arm (lines 175–222) + `compact_context` Err path (lines 634–651).

`compact_context` returns `None` in two semantically different situations: (a) summarization succeeded, and (b) summarization FAILED (the `Err(e)` arm emits `AgentEvent::Error` and then `return None`, line 650). `drive_turns` cannot distinguish them, so on a failed mid-turn compact it falls through to the success path: when there are no steers it pushes

```
[harness note] Context was compacted mid-task — continue from where you left off.
```

(lines 206–217) even though nothing was compacted, and runs the follow-up turn on the **uncompacted** (possibly still near-limit) conversation.

Two problems:

- The note asserts a falsehood to the model about its own context state ("Context was compacted" when the compaction failed).
- Pre-fix behavior on a failed compact was: emit Error, go idle. Resuming execution after a failed compaction is an unplanned behavior change on the error path — defensible (the interrupted task isn't lost), but it should be a conscious choice with an accurate message. If the failure was a provider outage, the immediate follow-up turn also burns `run_turn_with_retry`'s provider-error retries.

Suggested fix: have `compact_context` return a small enum instead of `Option<StopReason>` — e.g. `CompactOutcome::{ Completed, Failed, Stopped(StopReason) }` — and in `drive_turns` either go idle on `Failed` (restoring pre-fix behavior) or push an accurate note such as `[harness note] Context compaction failed — continuing with the original conversation.`

### 2. LOW — `summary_cut_index` can empty the kept tail; auto path then issues a system-only request

`src/agent/context.rs` — `summary_cut_index` (lines 318–341), used by `summarize` (146–149) and `summarize_with_interrupt` (229–232).

The helper only advances the cut forward. When the conversation ends in a trailing run of tool messages longer than the distance from the naive cut to the end — realistic for the auto-compact path, which fires at the top of the turn loop right after a large parallel tool batch (e.g. `assistant(tool_calls ×8)` followed by 8 tool results with `keep_recent = 6`) — the cut advances to `messages.len()`, `recent` is empty, and the summarized result is `[system, summary]` where the summary is **also** `Role::System` (context.rs:178–187, 299–308).

On the auto path (turn.rs:299 `*messages = summarized`, then the loop continues), the next provider request is built from only system messages. Strict providers (Anthropic requires ≥1 non-system message) reject it, ending the turn in provider errors — on a path whose whole purpose is recovering an over-full context. (Pre-fix this same scenario produced the reported guaranteed-wedge orphan rejection, so the change is strictly an improvement; the edge is just unhandled and untested.)

Suggested fix: when `recent` is empty (or the result contains no non-system message), append a synthetic user continuation message (the drive_turns harness-note text would do), and add a unit test for the all-tool-tail case asserting the result contains a non-system message. (Backing the cut *up* to keep the tool-owning assistant was considered and rejected here — it can retain arbitrarily large tool batches right after a compaction meant to shed them; advancing + synthesizing a user message keeps the token bound.)

### 3. LOW — Unpaired `CompactStarted` on the interrupt path (dangling "Compacting context…")

`src/agent/turn.rs:241–243` emits `CompactStarted` before the summarize call; the `Compacted` guard at 346 (`summarize_stop.is_none() && !compact_failed`) skips it when the user pressed Stop mid-summarization, and no `Error` is emitted on that path either. Same shape in `compact_context` (`src/runtime/agent.rs:621–623` emit; guard at 708). The transcript then keeps a dangling "Compacting context…" with no end note. The code comments say "the stop UX covers it" — if that refers only to the running/inflight indicator, the transcript entry itself stays dangling. Deliberate and documented, so low severity; consider emitting a small end note on interrupt (e.g. an error-kind "Compaction interrupted" entry) so the announcement always pairs, matching the invariant stated in the new doc comments ("every CompactStarted has a paired end").

### 4. LOW — Repeated auto-compaction now spams the transcript when it cannot reduce

`src/agent/turn.rs:225–353`. Auto-compaction re-fires on every turn-loop iteration while `token_count >= summarize_at`. When the summarization completes but doesn't get below the threshold — the too-few-messages early return (context.rs:224) with a few huge messages, or a summary that doesn't reduce enough — every iteration now appends **two** transcript entries ("Compacting context…" + "Nothing to compact — context is already summarized."). Pre-fix this no-reduction case was fully silent (the old `used < token_count` guard suppressed `Compacted`, and `CompactStarted` didn't exist). Bounded by tool-loop iterations (no tight spin), but a long agentic run wedged above the threshold accumulates note pairs. Consider a once-per-turn / cooldown guard on the announcement, or announcing only the first attempt.

### 5. LOW — Wording + doc-comment sync nits (constitution: documentation sync)

- `frontend/src/hooks/agentEventReducer.ts:678–682`: the no-reduction branch always says "Nothing to compact — context is already summarized." It also fires for the too-few-messages no-op (user pressed Compact on a 3-message conversation), where "already summarized" is confusing. Neutral wording, e.g. "Nothing to compact — already compact.", covers both cases.
- `src/runtime/agent.rs:598–611` (`compact_context` doc comment): describes ContextUsage emission but not the new `CompactStarted`/`Compacted` transcript announcements the function now makes.
- `src/agent/context.rs:131–134` (`summarize` doc, likewise `summarize_with_interrupt`): still says it keeps "the most recent `keep_recent` messages verbatim" — the kept tail can now be fewer than `keep_recent` (even empty) when the cut advances past orphaned tool messages. One sentence noting the boundary adjustment would keep the docs truthful.

## Checks that passed (no findings)

- **`summary_cut_index` boundary logic** (context.rs:335–341): no off-by-one — both callers guard `messages.len() <= keep_recent + 1` (141, 224), so the initial `len - keep_recent` cut is ≥ 2; the system message at `[0]` can never enter the kept tail; the `while` loop is bounds-checked (`cut < messages.len()`). An assistant-with-`tool_calls` cut point is valid in well-formed histories since its results immediately follow inside the kept tail. `to_summarize` (`messages[1..cut]`) is always non-empty.
- **`drive_turns` loop semantics** (agent.rs:162–225): cannot spin uncontrolled — the loop re-iterates only on `AfterTurn::Compact`, which requires an explicit user `/compact` mid-turn (turn.rs:676–690); auto-compaction stays inside turn.rs and never yields `AfterTurn::Compact`. Cancel → `return true` → break (pre-fix `break`) ✓; Clear → clear + idle ✓; Interrupt during compact → steers still pushed as user messages then idle — matches pre-fix behavior exactly ✓; follow-up turn is genuinely driven by the pushed steer user messages / harness note via the next loop iteration ✓. The two duplicated Prompt/Suggestion match arms were replaced correctly.
- **`compact_failed` flag** (turn.rs:268–295, 346): the Err arm emits `Error{retrying:true}`, keeps the original messages, and yields `stop = None` — without the flag a misleading `Compacted` would follow the `Error`; the flag suppresses it correctly. The `did_summarize = true` side effect on failure is acknowledged in the comment (forces one exact recount; harmless).
- **Event pairing**: `CompactStarted` precedes every summarize call on all four paths (idle `/compact` agent.rs:481→622; popup button shares that arm; mid-turn via drive_turns→compact_context; auto turn.rs:242). Ends are `Compacted` (any completed summarization, with or without reduction), `Error` (failure), or nothing on interrupt (finding 3). No path emits `Compacted` without a preceding `CompactStarted`. InputBar's optimistic push is removed, so the slash path and popup path announce identically — no double "Compacting context…".
- **channels.rs**: `CompactStarted` added to both enums, `into_serializable` mapping (629–634), roundtrip test asserting `{"kind":"compact_started"}`; wire kind matches `types.ts` and the new fixture (`event-compact-started.json`).
- **Frontend**: `reduceCompactStarted`/`reduceCompacted` are pure bookkeeping (`running` unchanged), correctly wired in `applyAgentEvent`; store tests + ipc-contract fixture test cover the new event and the no-reduction branch.
- **Constitution**: README bullet (line 55) accurately describes all three fixes; slash.ts help + hint updated; InputBar comment updated; PLAN.md's high-level compaction description (line 185–191) remains accurate at its level of abstraction. All new public items have doc comments; no `#[allow(...)]` added; no platform-specific APIs/paths/shell in the change. Regression tests would fail pre-fix: the orphan-pairing tests fail on the naive `len - keep_recent` cut, and `mid_turn_compact_resumes_execution_on_compacted_context` times out pre-fix (no third request / second `Finished`).
- **Security**: no new shell, network, filesystem, or approval surface; nothing flagged.

## Summary

The three planned fixes are correctly implemented and well tested. One medium finding (false "context was compacted" harness note + unplanned resume on the compaction-failure path, finding 1) and four low-severity findings (empty-tail edge producing system-only requests, dangling start announcement on interrupt, auto-compact transcript spam, and minor doc/wording sync). Test suites were reported green pre-review (cargo 1307/0, npm 495).
