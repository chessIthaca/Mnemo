# Review: ask_user redesign (collapse-to-answer, numbered options, command-box selection, subagent input lock)

**Date:** 2026-05-21
**Branch:** `feat/reviewing-workflow-state`
**Scope:** All uncommitted changes (`git diff HEAD` + untracked files) for the `ask_user` UX redesign.
**Verification run:** `npx tsc --noEmit` → exit 0; `npx vitest run` → 94/94 passed (incl. new `answerSelect.test.ts` 6 tests + 2 new store tests).

## Summary

The change is well-structured and the core logic is sound. The pure `parseNumericAnswer` helper is correct and well-tested. The collapse-to-answer flow (answered marker → `QuestionPrompt` returns `null` → `qa` transcript entry persists) works without duplication. The subagent lock correctly disables the textarea and hides Send while keeping Stop. Tests and type-check pass.

Findings are listed by severity below. The two **bugs** are real but low-severity edge cases; the rest are minor / informational.

---

## Correctness

**C1 — `lastAnswer` field is written but never read (dead state).**
`agentState.ts:142-148` (`lastAnswer`) and `agentEventReducer.ts:431-435` (`reduceQuestionAnswered` sets `next.lastAnswer`). A repo-wide search confirms `lastAnswer` is only ever *written* (in the reducer, `emptyAgentState`, `clearConversation`) and asserted in one test — it is never *read* by any component or reducer branch. The `qa` transcript entry is built directly from the `payload` passed to `recordQuestionAnswer`, not from `lastAnswer`. The doc comment on the field ("held so the reducer can append a `qa` transcript entry at answer time") describes a use that the implementation does not actually have — the entry is appended from `payload`, not from `lastAnswer`. This is harmless (no behavior bug) but is dead state that adds maintenance surface and a misleading comment. Consider either removing `lastAnswer` entirely, or wiring it as the single source the reducer reads (currently redundant with `payload`). **Severity: low (correctness/clarity).**

**C2 — Collapse-to-answer has no flicker/duplication; verified correct.**
`QuestionPrompt.tsx:48-52` returns `null` when `answeredId === question.questionId`, and the `qa` entry is appended in the same `recordQuestionAnswer` store mutation that sets `pendingQuestionAnswered` (`agentEventReducer.ts:425-430`). Because both happen in one `set()` call, React batches them into a single render: the live prompt disappears and the static `qa` entry appears atomically. No duplication, no flicker. The `key={question.questionId}` on `<QuestionPrompt>` (Conversation.tsx:118) is preserved, so the `useEffect` reset on question change still fires correctly. **No finding — clean.**

**C3 — `parseNumericAnswer` regex and `0)` case are correct.**
`answerSelect.ts:40` `/^(\d+)\)$/` correctly requires the trailing `)`, so a bare `2` (no paren) returns `null` and does not hijack ordinary numeric prompts — confirmed by test `answerSelect.test.ts:30`. The `0)` case: `n < 1` guard at line 43 returns `null` (test line 23 confirms). With `optionCount === 0` (no real options), `1)` maps to `freeform` (test line 34) — correct, since the only choice is "Let's talk about it". Multi-digit (`12)`, `13)`, `14)`) handled correctly (tests lines 38-40). **No finding — clean.**

---

## Bugs

**B1 — Freeform mode can get stuck if the question is answered/cleared by another path while in freeform mode.**
`InputBar.tsx:55-56` derives `inFreeformMode = freeformQuestionId !== null && freeformQuestionId === pendingQuestion?.questionId`. The `freeformQuestionId` is cleared on the next `started` event (`agentEventReducer.ts:105`, `reduceStarted`), so the normal answer-then-resume path is fine. **However**, consider this sequence: user picks the "Let's talk about it" number → `freeformQuestionId = q1` → before the user submits, the *same* question is answered via the `QuestionPrompt` button click (a different input device / a race with a second click) → `recordQuestionAnswer` sets `pendingQuestionAnswered = q1` and appends the `qa` entry, but does **not** clear `freeformQuestionId`. Now `pendingQuestion` is still non-null (cleared only on `started`), `freeformQuestionId === q1 === pendingQuestion.questionId`, so `inFreeformMode` stays `true`. The InputBar remains in freeform-answer mode with the banner showing, even though the question is already answered — typing + Enter would call `answerQuestion(q1, …)` again, which the backend rejects (returns `false`), so `recordQuestionAnswer` is skipped and the typed text is silently dropped (the `setText("")` at line 200 already cleared it). The user is left in a stuck freeform mode until the next `started`. 

This is a narrow race (requires answering the same question two ways before the agent resumes), and the backend's idempotency prevents corruption, but the UI gets stuck. **Fix suggestion:** in `reduceQuestionAnswered`, also clear `freeformQuestionId` (set to `null`) when recording an answer, so any in-flight freeform mode for that question is torn down. Alternatively, gate `inFreeformMode` on `pendingQuestionAnswered === null`. **Severity: low (edge-case UX bug, no data corruption).**

**B2 — Numeric-answer interception swallows a legitimate `N)` prompt when a question is pending.**
`InputBar.tsx:218-243`: when `pendingQuestion && !inFreeformMode`, any text matching `N)` is intercepted as a question answer. If the user genuinely wants to send the agent a prompt that happens to be exactly `2)` (e.g. a short numbered note), and a question is pending, the prompt is consumed as an answer (or switches to freeform mode) instead of being sent. This is an inherent tension of the "type `N)` to answer" design and is documented in the code comments, but it is a real behavioral surprise: the interception takes precedence over sending a prompt/steer. Out-of-range numbers (e.g. `99)` when only 3 options) correctly fall through to a normal send (`parseNumericAnswer` returns `null`). The only ambiguous case is an in-range `N)` that the user meant as a prompt. **Severity: low (design trade-off, documented).** No code change required, but worth noting that the user has no escape hatch (e.g. a leading space won't help since `parseNumericAnswer` trims). Consider documenting that `/`-prefixed or quoted input bypasses interception, if that is intended — currently it does not (the slash menu path returns before reaching the numeric check only when the menu is *open* with matches; a bare `/2)` would still be intercepted if no slash command matches).

---

## Security

**No findings.**

- All answer text flows through `answerQuestion` (tauri `invoke`) and is rendered via React's default escaping (`{entry.answer}`, `{entry.question}` in `Message.tsx:177-180` and `QuestionPrompt.tsx`). No `dangerouslySetInnerHTML`, no `eval`, no template-string HTML construction.
- `parseNumericAnswer` operates on trimmed text with a strict regex; no injection surface.
- The freeform answer is sent verbatim to the backend (which owns validation); the frontend does not interpret it as a command.
- The subagent lock is a UI guard, not a security boundary (the backend enforces command routing), which is the correct layering.

---

## Constitution compliance

**No findings.**

- This is all frontend TypeScript; the "doc comments on public functions" rule is Rust-only per the project constitution. TS code follows existing style (the new `parseNumericAnswer`, `recordQuestionAnswer`, `setFreeformQuestion`, and `reduceQuestionAnswered` all carry doc comments consistent with the surrounding code).
- No commits to `main`; changes are on feature branch `feat/reviewing-workflow-state`.
- Line-ending style preserved (edits were targeted `file_edit`-style diffs, no full rewrites detected).
- `vitest.config.ts` correctly adds the new test file to the include list (line 19).
- Existing code style (2-space indent, single quotes, trailing commas) is followed throughout the new code.

---

## Notes / informational (not findings)

- **Subagent lock reliability:** `agentParents[activeAgent]` is populated by `registerAgents` (`useAgentStore.ts:470`, `agentParents[info.id] = info.parent_id`) and cleaned up on agent exit (`agentEventReducer.ts:654-655`). The `isSubagent` computation (`InputBar.tsx:62-63`, `(agentParents[activeAgent] ?? null) !== null`) matches the existing subagent-detection idiom used in `useAgentEvents.ts:260,271` (`st.agentParents[payload.agent_id] != null`). Consistent and correct. The Stop button correctly remains available for subagents (`InputBar.tsx:696`, gated only on `running`, not `isSubagent`). **Clean.**
- **Stale-closure concern in `handleSend`:** `handleSend` reads `pendingQuestion` and `inFreeformMode` from the render closure. Because `handleSend` is recreated every render (it's a plain `async function` declared in the component body, not memoized), it always closes over the latest values at the time of the render that produced the current `onKeyDown`/`onClick` handler. React re-creates the handler on each render, so there is no stale-closure bug here. The `q = pendingQuestion` local capture (lines 199, 221) correctly snapshots the question for the async `await answerQuestion(...)` call. **Clean.**
- **`useEffect` reset in `QuestionPrompt`:** the `eslint-disable-next-line react-hooks/exhaustive-deps` on the question-change effect (line ~58) is intentional — it deliberately runs only on `question.questionId` change to clear stale freeform mode. This mirrors the pre-existing pattern. Acceptable.
