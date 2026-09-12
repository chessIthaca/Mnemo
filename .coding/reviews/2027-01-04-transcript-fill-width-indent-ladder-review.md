## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (`git diff HEAD` + untracked) for plan 6d51d819 — the transcript fill-width + indent-ladder change. The implementation is correct and complete: every entry kind sits at the right ladder level, the left-alignment mechanics are sound, there is no functional drift, and the design is documented and pinned by tests. One low test-robustness finding (F1) on the new layout test's whole-file absence assertions.

## Scope

- Diff: `frontend/src/components/chat/Conversation.tsx`, `frontend/src/components/chat/Message.tsx`, `frontend/vitest.config.ts`, `.coding/backlog.jsonl` (parent bookkeeping — af572504 in_flight→done from the prior plan-steps plan).
- Untracked: `frontend/src/components/chat/Conversation.layout.test.ts` (new), `.coding/plans/6d51d819.md`, `.coding/knowledge/spec/2027-01-04-plan-steps-ui-…md` (prior plan's spec).
- Read in full: both components, the new test, vitest.config.ts, ApprovalPrompt.tsx, QuestionPrompt.tsx, Conversation.test.ts, Message.tsx lines 500–1029 (ToolCard/CallDetail), PLAN.md:770–799, README transcript mentions. Independent searches verified the absence-assertion strings and absolute/sticky positioning.

## 1. Ladder correctness — PASS

All ten `MessageImpl` kinds accounted for; none missed, none double-wrapped:

- **Level 0** (flush left + `pt-[0.75em]`): user (Message.tsx:320), steer (:399).
- **Level 1** (`pl-[1em]`): assistant streaming (:344) + final (:354), error (:387 — wrapped, correct, its `px-4` would conflict with a direct pl), qa (:430 — wrapped).
- **Level 2** (`pl-[2em]`): tool (:381), skill (:411 — pl added to the existing `py-[0.125em]` div, no px conflict), memory (:448), vision (:457).
- Conversation.tsx: ApprovalPrompt (:127) + QuestionPrompt (:137) wrapped at `pl-[1em]`, `key` props preserved inside the wrappers.

Level assignments match the "who is speaking" rule (user prompts 0, agent prose 1, agent tool-work 2). The interactive prompts at level 1 are consistent with `qa` (their collapsed record) also at level 1 — the live QuestionPrompt and its persisted record sit at the same indent, so nothing jumps when the prompt collapses.

**Em soundness:** every pl wrapper is a plain div inheriting the scroll root's `fontSize: var(--app-font-size)` (Conversation.tsx:104); none carries a font-size class, so all indents resolve against the app font size uniformly — the ladder scales with the setting.

**Turn grouping via pt (padding), not mt** is the right call — Tailwind space-y's `> :not([hidden]) ~ :not([hidden])` margin-top would fight an `mt-*` on the child.

## 2. Left-alignment details — PASS

- user: `flex flex-col items-start` — the bubble is a flex item whose cross size is fit-content (shrink-to-fit, not stretched), and `whitespace-pre-wrap` wraps at the container width. Same for the images row (fixed `h-20 w-20` thumbs, `flex-wrap`) — no regression, just more thumbs per row at the wider edge.
- steer: `flex justify-start` — inner bubble content-sized, wraps at container via `whitespace-pre-wrap break-words`.
- `rounded-bl-sm` is the correct corner-cue flip for a left-aligned bubble (tail at bottom-left; was bottom-right).

## 3. Width-change side effects — PASS

- No absolutely-positioned/sticky elements inside the transcript column: the only `absolute` uses in the chat dir are InflightBar.tsx:326,345 — a separate bar, not a child of the column.
- The scroll container keeps `px-4 py-4` (Conversation.tsx:101); the column fills it.
- Prose already had `max-w-none` (Message.tsx:355) — fills the width. Long lines at full width are the explicit user request; the parent's plan correctly lists a ~100ch prose cap among future suggestions rather than sneaking it in.
- ToolCard internals use `overflow-x-auto` pre blocks (Message.tsx:952, 960, 1007, 1014, 1021) — wide content scrolls within the card, no container overflow.

## 4. No functional drift — PASS

- The show_tool_activity filter ternary (Conversation.tsx:108–114) is byte-identical to HEAD; the Conversation.test.ts pinned contracts are untouched and still hold.
- No store/IPC/data changes anywhere: the diff's only non-frontend hunk is `.coding/backlog.jsonl` (prior plan's status flip — parent bookkeeping).
- ApprovalPrompt/QuestionPrompt `key` props preserved inside the new pl wrappers (reset behavior intact).

## 5. Test quality — PASS with one low finding (F1)

- Registered: frontend/vitest.config.ts:19, next to Conversation.test.ts.
- Contracts pin the design: width fill (no `max-w-4xl`, `space-y-[0.5em]` present, no `max-w-3xl`), alignment (`items-start`/`justify-start` present), corner cue (`rounded-bl-sm` present, `rounded-br-sm` absent), turn grouping (`pt-[0.75em]`), ladder (`pl-[1em]` in both files, `pl-[2em]`).
- Absence assertions verified safe today by independent search: zero occurrences of `items-end`/`justify-end`/`rounded-br-sm`/`max-w-3xl`/`max-w-4xl` in Message.tsx or Conversation.tsx (the only chat-dir hits are the test file's own assertion lines).
- Style matches the repo's established `?raw` source-contract convention (Conversation.test.ts does the same, including absence assertions).

## Findings

### F1 (LOW) — whole-file absence assertions over the 1029-line Message.tsx are broader than the contract they pin

`frontend/src/components/chat/Conversation.layout.test.ts:33-34`:

```ts
expect(messageSource).not.toContain("items-end");
expect(messageSource).not.toContain("justify-end");
```

Message.tsx is not just the prompt bubbles — it also contains ToolCard, CallDetail, MemoryEntryCard, VisionEntryCard, and the argLabel/displayName helpers (1029 lines total). Any future legitimate use of `items-end`/`justify-end` anywhere in those components (e.g. a bottom- or right-aligned chip row in ToolCard) would spuriously fail the test named "left-aligns user messages and steers", with a failure message pointing at prompt alignment. Conversation.test.ts's absence assertion (`not.toContain("showMemoryActivity")`) is naturally scoped because Conversation.tsx is a 148-line file about that one filter; Message.tsx is not.

**Fix (one-liner, keeps the regression guard exactly as tight):** scope the absence to the old prompt class strings — `not.toContain("flex flex-col items-end")` (user's old outer div) and `not.toContain("flex justify-end")` (steer's old outer div). The other absence assertions (`rounded-br-sm`, `max-w-3xl`, `max-w-4xl`) are bubble/width-specific enough that whole-file absence IS the intended contract — leave them.

## Constitution checks

- **(a) Doc sync — PASS.** The ladder is documented at Message.tsx:309-316 (above the switch: the level rule, the em rationale, the Conversation.tsx companions) and in the test file's doc comment (lines 5-13). README.md mentions the transcript only in feature contexts (shell filtering :36, compaction :64, image parsing :68, context menu :70) — no layout/alignment claims to update. PLAN.md:781-782 lists the chat components generically ("Conversation (scrollable transcript), Message (user / assistant / tool / error)") — nothing stale. Code-level documentation is sufficient for a pure CSS-class display change; no README/PLAN.md update warranted.
- **(b) Multi-platform neutrality — PASS.** Frontend-only (zero changes under src/ or src-tauri/), no platform APIs, no `cfg(windows)`, no paths or shell syntax.
- **(c) Warning-free — PASS.** No new imports in the modified components (Compass still used at Message.tsx:401); the test file's imports (describe/expect/it + two `?raw` sources) are all used. Parent reports `npx tsc --noEmit` clean and 849/849 tests green (62 files, +7 contracts).
- **(d) Public-function doc comments — unaffected.** No new public functions; MessageImpl is internal (the memo export is unchanged).

## Notes (no action needed)

- The first transcript entry (usually a user prompt) now carries `pt-[0.75em]` with no preceding sibling, so it sits 0.75em lower than before (the scroll root has py-4). Uniform turn grouping — imperceptible and deliberate.
- The untracked `.coding` files (plan 6d51d819.md, the prior plan's spec) and the backlog.jsonl status flip are parent bookkeeping and belong in the commit alongside the code.
