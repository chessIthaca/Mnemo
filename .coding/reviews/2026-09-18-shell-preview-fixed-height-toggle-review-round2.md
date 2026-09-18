## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of plan 90c15456 (backlog 6f25fb7e) on `wt/mnemo` — the full uncommitted changeset (round-1 fix + the L1/L2 fix round). Both round-1 LOWs are genuinely fixed: the L1 arithmetic is now exactly right (h-[10em] = six lines at the element's own em, confirmed by independent computation), every comment/test/doc claim is consistent with the delivered class, and the L2 comment correction landed verbatim in Message.tsx. No regression anywhere in the round-1-verified surface. One residual LOW: the same L2-class misattribution ("Only shell streams (the reducer's neverGroups)") survives in a comment in the NEW test file Message.preview.test.tsx:61 — the Message.tsx instance was corrected but this sibling was not.

## L1 — h-[10em] arithmetic: VERIFIED CORRECT

Independent recomputation against the delivered source (Message.tsx:690-698, class at :694):

- Parent font-size F; the pre's `text-[0.75em]` → element font-size 0.75F (em in font-size is parent-relative).
- `h-[10em]` and `p-[0.5em]` resolve against the ELEMENT's own font-size (em in any property other than font-size is element-relative) = 0.75F; Tailwind preflight `*{box-sizing:border-box}` puts the padding inside the height.
- Height = 10 × 0.75F = 7.5F; vertical padding = 2 × 0.5 × 0.75F = 0.75F; content = 6.75F.
- Line-height: no `leading-*` on the pre or any chat-column ancestor, and no `line-height` rule anywhere in frontend/src (the only matches are comments — including the corrected one itself); preflight's `html{line-height:1.5}` inherits unitless → 1.5 × 0.75F = 1.125F per line.
- 6.75F ÷ 1.125F = **exactly 6 lines** — matching the task's stated arithmetic and the comment's element-em sum (6 × 1.5em + 2 × 0.5em = 10em).

Comment accuracy (Message.tsx:680-689): "the block occupies h-[10em] — six lines at the element's own em (6 × 1.5em line-height + 2 × 0.5em padding; the pre's text-[0.75em] makes every em below font-size element-relative)" — every clause true, including the element-relative-em explanation. Stick-to-bottom comment (:562-563) says "10em here" ✓. FEATURES.md:25 "occupies a FIXED height (six lines)" — now true ✓. (Note, not a finding: with wrapped long lines, 6 logical window lines can exceed 6 rendered lines and clip the top — scrollable, stick-to-bottom keeps the newest visible; identical trade-off to the round-1-endorsed fix direction.)

## L2 — comment attribution: FIXED in Message.tsx

Message.tsx:686-687 now reads "Only `shell` emits ToolOutputDelta (the shell.rs sink), and shell never merges (the reducer's neverGroups), so the reservation is shell-only" — exactly the requested correction: the sink is named as the source of streaming, neverGroups correctly attached to the merge half. The pre-existing :549-556 comment (round-1-judged correct) is untouched. ✓

## LOW 1 (new) — the L2-class misattribution survives in Message.preview.test.tsx:61

`// Only shell streams (the reducer's neverGroups) — no reservation for any other tool, even with a stray liveOutput.`

The parenthetical attaches neverGroups — the MERGE policy (agentEventReducer.ts:557) — to "only shell streams", the exact compression round 1's L2 flagged in Message.tsx: the reason only shell streams is the shell.rs ToolOutputDelta sink; neverGroups explains why the shell call keeps its own card. The comment existed at round 1 (the file is new in this changeset) but only the Message.tsx instance was in L2's cited scope, so the fix round corrected that one and left this sibling. Doc-accuracy nit only, no behavior implication — the test itself is correct. Fix: one line, e.g. `// Only shell emits ToolOutputDelta (the shell.rs sink) — no reservation for any other tool, even with a stray liveOutput.`

## Consistency sweep — no stale values

- `7.75`: the only remaining occurrences are historical records, not live claims — the plan file's original design sketch and ticked step text (superseded by its own round-1 record at .coding/plans/90c15456.md:12, which documents the fix to h-[10em]) and the round-1 review report quoting the old value (as it must). No code, test, comment, or doc asserts 7.75em as current. ✓
- `12em`: only the two intentional negative pins (`not.toContain("max-h-[12em]")` in Message.preview.test.tsx:49 and toolCardPaths.test.ts:1175). ✓
- h-[10em] pinned positively at Message.tsx:694 (class), the toolCardPaths.test.ts contract, and Message.preview.test.tsx:47/:56; pinned negatively for the gated-off cases (:67, :86, and Message.preview.off.test.tsx:46). ✓

## No regression in the round-1-verified surface

- Gate intact: `{showShellPreview && name === "shell" && running && (` (Message.tsx:690), selector at :446-448 — and the contract test's `toContain` string matches the source byte-for-byte.
- Reservation semantics unchanged: the block renders for every running shell call from first paint, empty until the first chunk; the only variable content (liveTail) sits inside the fixed box.
- Toggle wiring complete and unchanged: appearance.ts `LS_SHOW_SHELL_PREVIEW` + `readShowShellPreview()` (default true, mirrors readShowTokenUsage); useAgentStore state + setter (writeLs + set) + init + import/re-exports; types.ts ChatDraft + localStorage-only doc; ChatSection draft capture/apply (deviation comment)/checkbox; the serializeChat dirty-detection case in ChatSection.test.ts; the appearance.test.ts reader cases.
- Both new test files registered in vitest.config.ts's include allowlist.
- FEATURES.md + ChatSection doc comment synced; the only other change is .coding/backlog.jsonl bookkeeping (status transitions) — expected.

## Test battery

Not re-runnable from this read-only session (no shell tool). Accepted as stated evidence, consistent with the plan file's round-1 record (.coding/plans/90c15456.md:12 — full vitest suite green, tsc + vite build clean, cargo test 2410 passed / 0 failed / 5 ignored + 16 integration). Code-level consistency of every pinned assertion was verified against the actual source (gate string, class, aria-live, scrollTop idiom, liveTailPreview, calls.find, negative pins), so the pinned tests pass on the delivered source.
