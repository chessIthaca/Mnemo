# Review: backlog line-break display (feat/backlog-linebreaks)

**Date:** 2026-08-21
**Scope:** all uncommitted changes on `feat/backlog-linebreaks`
**Changed files (git status / git diff HEAD):**
- `frontend/src/components/views/BacklogView.tsx` — 2 className additions (`whitespace-pre-wrap break-words` on the card prompt display div, line 561, and the failure-note div, line 589)
- `frontend/src/components/views/BacklogView.test.ts` — new `describe("BacklogView line-break display")` source-contract block, 3 tests
- `.coding/plans/stack.json` (+ untracked `.coding/plans/33865952-….md`) — workflow bookkeeping, not part of the feature

## Findings

### M1 (medium) — Card-display regression test is satisfied by the pre-existing hover-preview line, so it would NOT fail if the fix were reverted

`frontend/src/components/views/BacklogView.test.ts:49-51` asserts:

```ts
expect(source).toContain(
  "whitespace-pre-wrap break-words text-[0.8em] text-slate-200",
);
```

That exact substring also appears in the **pre-existing, unchanged** hover-preview portal className at `BacklogView.tsx:602` (`<div className="whitespace-pre-wrap break-words text-[0.8em] text-slate-200">`), which already had these classes before this change (the diff touches only lines 561 and 589). The hover preview renders only for items with text > 200 chars or images, after a 400 ms hover — so the primary card display path could regress completely (newlines collapsing to spaces again) and this test would still pass. This violates the project rule (agent.md): "The test must fail without the fix and pass with it."

The test's own comment says the classes "must sit on the display path, not only in the hover preview" — precisely the distinction the assertion fails to make.

**Fix:** assert a substring unique to the template-literal card div, e.g. the full literal including the backtick:

```ts
expect(source).toContain(
  "`whitespace-pre-wrap break-words text-[0.8em] text-slate-200 ${isLong ? \"cursor-pointer\" : \"\"}`",
);
```

or at minimum `` "`whitespace-pre-wrap break-words text-[0.8em] text-slate-200 ${isLong" `` — the backtick opener cannot match the static `className="..."` on line 602.

Test 2 (`mt-1.5 whitespace-pre-wrap break-words rounded border border-border`, test.ts:55-57) is unique to the note div (line 589) and correctly fails on revert. Test 3 (test.ts:60-64) pins pre-existing truncation/expand strings — fine as a guard, and not the regression test for this fix. After fixing M1, the pair (fixed test 1 + test 2) gives real fail-on-revert coverage for both changed divs.

## Verified correct (no findings)

1. **Correctness of the fix itself.** `whitespace-pre-wrap` preserves `\n` as line breaks while still wrapping at word boundaries; `break-words` (overflow-wrap: break-word) prevents overflow from long unbroken tokens. This is the same class pair the hover preview already used (line 602), so the list display is now consistent with the preview. Both divs changed are exactly the two paths that collapsed newlines (card prompt text, line 565 via `displayText`; failure note, line 590 via `item.note`). The other render paths are unaffected and correct: the hover preview (line 602-604) already preserved newlines; the edit textarea (line 522-539) preserves them natively. No other render of `item.text`/`item.note` exists in the file (clipboard copy, line 266, and editor state, lines 347/353/385, don't render).
2. **Truncation/expand logic still sound.** `isLong`/`displayText` (lines 254-255) are char-based and unchanged; pre-wrap doesn't alter the 200-char slice or the `…` ellipsis, long single-line prompts still wrap instead of overflowing, and the click-to-expand affordance (`onClick`, `cursor-pointer`, `title`, lines 561-563) is intact. A mid-line slice is pre-existing behavior, not a regression.
3. **Template literal still valid.** Line 561's interpolation `${isLong ? "cursor-pointer" : ""}` is unchanged and valid; a trailing space in the non-long case is harmless. `title` semantics unchanged.
4. **Security.** CSS classes only; static strings, no user-controlled input; React escapes all rendered text (`{displayText}`, `{item.note}`). No injection surface introduced.
5. **Multi-platform neutrality (agent.md).** Pure frontend Tailwind CSS (`white-space`/`overflow-wrap` are standard, cross-platform CSS); no Windows/macOS-specific APIs, paths, or syntax. Compliant.
6. **Documentation sync (agent.md).** Cosmetic CSS change; README.md/PLAN.md do not enumerate per-card Tailwind classes and need no update. The new test block carries an explanatory doc comment consistent with the file's style. No doc finding.
7. **Build/test hygiene.** No new imports, variables, or dead code in `BacklogView.tsx` (two className string edits only); the test file reuses the existing `source`/`describe`/`expect`/`it` imports (no unused imports), so `npm run build` (tsc && vite build) stays warning-free. Tailwind utilities `whitespace-pre-wrap`/`break-words` were already present in the source (line 602), so the JIT scan already emits them — no missing-CSS risk. The source-contract test style matches the file's existing no-DOM vitest convention (`node` env), correctly avoiding a React DOM harness.

## Conclusion

One medium finding (M1): the card-display regression test would not fail if the primary fix were reverted, because the pre-existing hover-preview className satisfies the assertion. Fix it by asserting the template-literal form unique to the card div, then re-run `npm test` / `npm run build`. Everything else is correct and compliant.
