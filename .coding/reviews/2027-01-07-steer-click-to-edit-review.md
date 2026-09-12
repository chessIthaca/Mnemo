## Verdict: FINDINGS (0 high, 4 low)

Review of ALL uncommitted changes on `wt/agenticcoding` for plan d8c74589 (click-to-edit a pending steer, draft mode): the InputBar.tsx edit affordance + shared `cancelPendingSteer` helper, the new `InputBar.steerEdit.test.ts` source-contract suite, the `vitest.config.ts` include entry, plus bookkeeping (`.coding/plans/d8c74589.md`, backlog `in_flight` stamp). The implementation is correct and behavior-preserving for the shipped X half; the mid-edit-landing and stale-state reasoning holds (one nuance → LOW 2). The four findings are low-severity: one cosmetic UX (Pencil clipped for long texts), one comment-accuracy overstatement, one test-anchor fragility (the bfbab877 class), one documentation-sync reminder for close-out. Details below.

## Scope & method

Reviewed `git diff HEAD` (3 modified: `.coding/backlog.jsonl`, `frontend/src/components/layout/InputBar.tsx`, `frontend/vitest.config.ts`) plus both untracked files (`.coding/plans/d8c74589.md`, `frontend/src/components/layout/InputBar.steerEdit.test.ts`). Cross-checked against the supporting surface: `agentState.ts` (SteerEntry, exported interface :138), `useAgentStore.ts` (removeSteer subscription, suggestion_injected dispatch :1100-1132), `agentEventReducer.ts` (reduceSuggestionInjected :1060-1081), `lib/tauri.ts` (cancelSuggestion :63, documented no-op-if-already-injected), `lib/promptHistory.ts` (resetNavigation :268), `InlineMarkdown.tsx` (phrasing-only output), the pre-existing `?raw` contracts on InputBar.tsx (`App.shellRender.test.ts`, `InputBar.test.ts`, `attachImages.test.ts`), `vitest.config.ts`, the X SPEC knowledge file, and README's steer mentions. Every assertion in the new test file was statically verified present/absent in the current source — the 4 tests provably pass against the shipped source.

## Verified correct

1. **Helper extraction preserves the X's exact behavior.** `cancelPendingSteer` (InputBar.tsx:659-665) is line-for-line the old inline handler: `activeAgent === null` guard → `removeSteer(activeAgent, steer.id)` → `cancelSuggestion(activeAgent, steer.text).catch(console.error)`. The X button's onClick now calls it; nothing else about the X changed. Doc comment present (project rule).
2. **Edit-handler wiring.** `setText(steer.text)` / `resetNavigation()` / `cancelPendingSteer(steer)` / `textareaRef.current?.focus()` — correct order, and `resetNavigation` is *required*: programmatic `setText` bypasses onChange (which normally resets navigation on typing, InputBar.tsx:836), and it also drops the saved draft — consistent with decision (c) (click overwrites the draft; the Up-arrow precedent at :627-639 does the same). The extra `activeAgent === null` guard in the onClick is redundant with the helper's but harmless.
3. **JSX/HTML validity.** Edit button and X are siblings (no nested buttons); `type="button"` on the edit button (matches the slash-menu buttons); InlineMarkdown is verified phrasing-only (`strong`/`del`/`em`/`code`/text — its doc comment guarantees no block elements), so it is valid inside a `<button>`; the landed branch keeps the plain truncate span + "landed" label.
4. **Landed-not-editable guarantee.** The landed branch renders no button/onClick/cancel (pinned by test 4); after landing the bubble auto-removes via the steerId-scheduled timer (useAgentStore.ts:1115-1131). No edit state exists anywhere — the "draft" is just the input text — so nothing can go stale after injection. ✓
5. **Mid-edit-landing hazard.** The cancel-at-click reasoning holds for the editing window: once the backend processes `cancel_suggestion` the queue no longer holds the steer, nothing can land mid-edit, and a re-send re-queues cleanly through the existing steer branch (handleSend steerMode → pushPrompt + addSteer + sendSuggestion, :307-316). One residual nuance → LOW 2.
6. **Missing-steer race safety.** If `suggestion_injected` arrives after the click already removed the bubble, `reduceSuggestionInjected` finds no pending steer (agentEventReducer.ts:1065-1074): no ghost landed bubble, no removal scheduled; the injection still shows as a `steer` transcript entry. No crash, no corrupt state.
7. **Layout.** `min-w-0 flex-1` on the button makes truncation work inside the flex row; the X's `ml-auto` is redundant with `flex-1` but harmless (pre-existing class kept); the landed span's `truncate` alone suffices (overflow:hidden zeroes the flex automatic min-width); `group` only on pending bubbles — the only place the Pencil's `group-hover` applies.
8. **Tests are meaningful.** Test 3's load-bearing anchor is `title="Cancel this steer"` (unique to the steer X — the `<X className="h-3 w-3" />` assertion alone would also match the image-remove X, but the pair is sound); test 4's end anchor `") : ("` correctly delimits the landed ternary (the first occurrence after the landed comment); the vitest.config.ts entry is the exact right path and matches the enumerated-file convention (the silent-skip hazard is real — no glob in the config covers it).
9. **No collateral breakage.** The other `?raw` importers of InputBar.tsx all still hold against the new source: `App.shellRender.test.ts` (`= memo(function InputBar()`), `InputBar.test.ts` (`<InlineMarkdown text={steer.text} />` present in both branches; `<span className="truncate">` still in the landed branch; the handleSend→handleStop slice anchors intact), `attachImages.test.ts` (paste wiring untouched).
10. **Security.** No surface: InlineMarkdown renders React-escaped nodes only (no raw HTML/dangerouslySetInnerHTML); the title is static; IPC carries plain text. Multi-platform neutral (frontend-only, no platform assumptions).

## Findings

### LOW 1 — Pencil affordance is clipped for long steer texts (UX/cosmetic)

**Problem:** the edit button carries `truncate` (overflow:hidden + white-space:nowrap) and the Pencil is the LAST inline child, after the text. For any steer text wider than the button (the common case — bubbles truncate for a reason), the pencil is entirely clipped: invisible at rest AND on hover. `shrink-0` on the Pencil is inert (the button is not a flex container). The remaining discoverability signals are the pointer cursor, the hover color change, and the title tooltip — workable, but the intended hover affordance silently disappears exactly when the text is long.

**Fix:** make the button the flex row and move truncation onto an inner span so the pencil sits outside the clipping box:

```tsx
<button type="button" onClick={/* unchanged */}
  className="flex min-w-0 flex-1 cursor-pointer items-center text-left transition-colors hover:text-amber-300"
  title="Edit this steer — loads it into the input; send re-queues it"
>
  <span className="truncate"><InlineMarkdown text={steer.text} /></span>
  <Pencil className="ml-1 h-2.5 w-2.5 shrink-0 opacity-40 transition-opacity group-hover:opacity-100" />
</button>
```

(drop the now-inert `inline` class on the Pencil; `shrink-0` becomes meaningful; the inner span's overflow:hidden lets it shrink in the flex row). **Tests:** the existing assertions survive (`title="Edit this steer`, `<Pencil className=`, the handler slice); optionally pin the inner truncating span.

### LOW 2 — the "BY CONSTRUCTION" comment overstates the cancel-at-click guarantee

**Problem:** the handler comment says "Cancel-at-click means nothing can land mid-edit (the backend queue no longer holds the steer)". `cancelSuggestion` is async IPC: between the click and the backend processing the cancel, the steer is still queued and can be injected at a turn boundary. In that window the bubble is already removed, so the landing is visible only as a transcript `steer` entry — and the user's edited send re-queues the guidance, so the agent sees it twice. This race is identical to the X flow's (pre-existing class; the SPEC documents the backend cancel as a no-op if already injected), so it is inherited, not introduced — but the comment claims more than the mechanism delivers, and the plan asked for this reasoning to be scrutinized.

**Fix (comment-only):** soften to e.g. "once the backend processes the cancel, the queue no longer holds the steer, so nothing can land mid-edit; the residual click-instant race (cancel arrives after injection) is the X flow's pre-existing IPC race — the landing still shows as a transcript steer entry". No code change needed.

### LOW 3 — test slice bounds anchor on prose comments (the bfbab877 fragility class)

**Problem:** tests 2 and 4 locate their slices via comment text (`"// Pending — click-to-edit (draft mode)"`, `"// Landed — injected; NOT editable"`). A comment reword makes `indexOf` return -1 → `slice(-1, …)` → empty → the assertions fail loudly (safe, but exactly the reword→break→re-pin cycle the 2026-12-06 / 2027-01-06 quality reviews drove out of the run_all.rs source-contract tests, backlog bfbab877). Test 2 also doesn't assert its start anchor was found (test 4 does — `landedStart > -1`), so its failure would read as four missing `toContain` strings rather than "anchor not found".

**Fix:** anchor structurally. The established pattern in this file family is index-order assertions (InputBar.test.ts's `slashCheckPrecedesSteerBranch`): e.g. assert `indexOf("setText(steer.text);") > -1` and `< indexOf('title="Edit this steer')` (the handler precedes its button's title), keeping the co-presence strength without prose anchors; or keep the comment anchor but assert `> -1` explicitly and mark the phrase as the contract per the bfbab877 guidance. **Tests:** the re-anchored test must still fail when the handler is gutted (spot-check by deleting the `setText` line) and survive a comment-only reword.

### LOW 4 — documentation sync: durable knowledge file for click-to-edit at close-out

**Problem:** the X feature has a knowledge SPEC (`.coding/knowledge/spec/2026-08-30-cancel-delete-a-scheduled-steer-via-the-x-on-eac.md`); click-to-edit reuses that flow but its draft-mode design (cancel-at-click, re-queue-on-send, landed-not-editable) currently lives only in code comments + the plan file. The main agent plans a SPEC memory at close-out — the close-out should also extend the X SPEC (or add a companion spec file) so the design survives in the git-tracked knowledge layer, for parity with the X feature. README needs nothing (it never listed the X affordance either; its steer-bubble mention — line 78, InlineMarkdown surfaces — remains accurate).

**Fix:** at close-out, extend the X SPEC file with the click-to-edit draft-mode flow (or add `.coding/knowledge/spec/2027-01-07-click-to-edit-a-pending-steer-draft-mode.md`) covering: the affordance, cancel-at-click + re-queue-on-send, landed-not-editable, and the inherited text-based-queue limitation. No code change.

## Constitution checks

- **Documentation sync:** LOW 4 (code comments are thorough; knowledge-file parity pending at close-out).
- **Multi-platform neutrality:** PASS — frontend-only, no platform assumptions, no paths/shell/OS APIs.
- **Code style:** PASS — matches InputBar.tsx's idiom; helper doc-commented; tsc exit=0 reported (no warnings surface on the frontend side).
- **Regression-test rule:** N/A (feature, not a bug fix) — the 4 new contracts pin the feature; test 3 durably answers "the user asked for the X as if missing" (it shipped and stays wired).
- **Test evidence:** reported vitest 71 files / 1002 tests green + `tsc --noEmit` exit=0 + both Rust suites green (frontend-only diff — Rust untouched, consistent with the diff). Statically cross-verified: every assertion in the new test file holds against the current source, and no other `?raw` contract on InputBar.tsx breaks.
