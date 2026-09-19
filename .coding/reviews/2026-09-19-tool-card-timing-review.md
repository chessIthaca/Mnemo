## Verdict: PASS

Review of ALL uncommitted changes on `wt/mnemo` for implementation plan 850862a7 ("Tool-card timing: duration + wall-clock next to the check"). The change is correct, minimal, matches the plan, and is fully covered by tests. 0 high, 0 low findings; a few informational notes below (none require action).

**Scope reviewed:** the full `git diff HEAD` (types.ts, agentEventReducer.ts, Conversation.tsx, Message.tsx, useAgentStore.test.ts, Message.preview.test.tsx, vitest.config.ts, .coding/plans/3f87686a.md) plus the untracked new files (frontend/src/lib/timeFormat.ts, timeFormat.test.ts, .coding/plans/850862a7.md, and the prior plan's knowledge side-car files).

## Correctness verification

**Reducer stamping (agentEventReducer.ts)** — `startedAt: Date.now()` is stamped on the invocation literal in BOTH branches of `reduceToolCallStart` (merge into an existing card, and new-entry push); `endedAt: Date.now()` is stamped in `reduceToolResult` on exactly the call matched by `c.id === event.tool_call_id`. Merge semantics are untouched: `neverGroups` (shell/search/search_read/browser), the failed-call chain break (`lastCallFailed`), `MAX_CALLS_PER_TOOL_CARD`, the memory-entry path, and `capTranscript` are all byte-identical to before — the diff adds only the stamp fields. The memory path correctly gets no timing (per plan).

**fmtTs moved verbatim** — the body in timeFormat.ts:19-24 is character-identical to the removed Conversation.tsx definition; Conversation.tsx now imports it (line 10) and the `title={chatHoverTimestamps && entry.ts !== undefined ? fmtTs(entry.ts) : undefined}` usage (lines 203-207) is unchanged. Conversation.layout.test.ts's source assertions (`chatHoverTimestamps` :86, `fmtTs` :87) still hold against the current source — verified by direct read.

**fmtToolDuration boundaries** — hand-verified: 0→"0ms", 999→"999ms", 1000→"1.0s", 2300→"2.3s", 59_900→"59.9s", 60_000→"1m 0s", 61_000→"1m 1s", 72_500→"1m 13s" (Math.round(12.5)=13, half-up), 119_999→m=1,s=round(59.999)=60→carry→"2m 0s". The 60s carry is correct and pinned by test.

**Render gating (Message.tsx)** — timing lives inside the completed-state span only; the running branch (thinking dots) is untouched. Duration = LAST call's `endedAt−startedAt` (consistent with the card's last-call-outcome rule), wall-clock = FIRST call's `startedAt` (timeline anchor). Each segment renders only when its data exists (`!== undefined`), so legacy invocations render exactly as before. CallDetail appends per-call timing to the `#N — label` row (grouped cards only — single-call cards get their timing in the header, per plan step 4). Styling stays inside the existing `text-[0.75em] text-slate-500` span.

**Legacy/persisted round-trip** — `startedAt?`/`endedAt?` are optional with doc comments; nothing requires them: the only consumers are the `!== undefined` render gates and the new tests. A restored legacy RUNNING call whose result lands in a new session gets `endedAt` without `startedAt` → both segments stay hidden (duration gate requires both) — graceful. The exact `toEqual` in useAgentStore.test.ts was updated to `startedAt: expect.any(Number)`; `toEqual` treats the absent `endedAt` key correctly.

**Empty calls array** — `case "tool"` dispatches to ToolCard unguarded, but an empty `calls` array would already crash pre-change at the `lastCall.result` computation (Message.tsx:458-459) before reaching the new `calls[0]` access — and it is unreachable in practice (the reducer always creates ≥1 call; persisted transcripts are reducer output). Not introduced by this change.

**Tests** — the two new reducer tests cover both branches (single event → new-entry; second event → merge) and per-call stamp distinctness on merged cards; the three preview tests cover stamped/legacy/running with a deterministic 2300ms duration; timeFormat.test.ts pins the boundaries listed above. `src/lib/timeFormat.test.ts` is registered in vitest.config.ts test.include — the vitestInclude guard (which discovers every `*.test.{ts,tsx}` under src/ via import.meta.glob and fails on any unregistered one) is satisfied by the literal-path entry.

## Project-specific checks

- **Documentation sync** — verified: README.md has no chat/tool-card feature list (one philosophical "chat window" mention only); no doc update needed — I agree with the assessment. The new lib and the new type fields carry doc comments; no config surface was added.
- **Multi-platform neutrality** — pure frontend TS; `Date.now()`, `toLocaleTimeString`, `toLocaleString` are platform-neutral. No platform-specific code.
- **File-tools-first** — no shell-based file mutation anywhere in the diff; all changes are source/test/config edits consistent with file_edit/file_write.
- **Security** — no new input parsing or injection surface; the formatters are pure and React escapes their output.

## Notes (informational, no action required)

1. **Legacy+stamped merged card**: if a conversation restored from a legacy save continues and a new same-tool call merges into an old completed card, the header shows the new call's duration but no wall-clock (calls[0] is legacy, unstamped). This is per-spec ("each only when its data exists", anchor = FIRST call's startedAt), degrades gracefully, and the expanded per-call rows still show the new call's timing. Rare edge; acceptable.
2. **Format seam at ~60s**: a 59_950–59_999ms duration renders "60.0s" (one-decimal rounding) rather than "1m 0s" — accurate to the stated precision and per the plan's specified format; not incorrect.
3. **Prior-plan bookkeeping rides along**: the uncommitted tree also carries the 3f87686a plan amendment and its knowledge bug/spec files (the already-landed switch-restart fix). These are `.coding/` side-car files designed to travel with git; committing them with this change is fine — just mention their inclusion in the commit message, or commit them separately.
4. The code-graph index still resolves `fmtTs` to the old Conversation.tsx definition site — a stale rebuildable cache (gitignored), not part of this change; it converges on next re-index.

## Verification status

The task reports `cd frontend; npm test` fully green (including the new tests and the vitestInclude guard) and `cargo test` 2492 passed / 0 failed (Rust untouched). As a read-only reviewer I verified the logic by direct source reads (reducer semantics, formatter boundaries, render gating, test assertions, config registration, layout-test contracts) — all consistent with the reported green runs.
