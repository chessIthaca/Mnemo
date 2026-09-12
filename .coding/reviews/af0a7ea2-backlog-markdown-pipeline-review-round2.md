## Verdict: PASS

Round-2 re-review of plan af0a7ea2 ("Render backlog item text with the plan display's markdown pipeline"). Both round-1 findings are resolved exactly as prescribed, the fixes are correctly scoped (no behavior change for any other chip kind or rendered surface), the new/updated tests genuinely pin the fixes, and the remainder of the uncommitted set is benign bookkeeping. No new issues found.

## Finding 1 (low — fallback newline collapse): RESOLVED

- **Fix verified.** `frontend/src/components/chat/Markdown.tsx:23` — `MarkdownFallback` now renders `<div className="markdown-fallback whitespace-pre-wrap break-words">`, the exact second option the round-1 report prescribed. The doc comment (lines 18-21) explains the lazy-load-window rationale and references the 2026-08-21 regression; `break-words` matches the rendered path's overflow behavior.
- **Scope is correct — fallback only, not the rendered pipeline.** `Markdown.tsx:48-54`: the fallback div exists only as the Suspense `fallback`; once the lazy chunk resolves React unmounts it and renders `MarkdownImpl` in its place. No inheritance path carries pre-wrap into rendered output, and on the backlog surfaces remark-breaks consumes `\n` into `<br>` anyway — no double-break is possible.
- **Inaccurate comment corrected.** `frontend/src/components/views/BacklogView.test.ts:38-48` now states the accurate rationale: no literal newline survives the rendered output (pre-wrap there would be a no-op, not a double-break) and the fallback owns newline visibility during the lazy window via its own classes. This closes the round-1 sub-point about the "double-break" justification.
- **Regression test pins it.** `frontend/src/lib/markdownRendering.test.ts:101-112` — new describe "Markdown fallback (lazy-load window) source contract" asserts the raw source contains `className="markdown-fallback whitespace-pre-wrap break-words"` via the established `?raw` import pattern (line 10). Removing either utility fails the test. Consistent with the reported +1 test count (763 → 764).
- Side effect checked: the fallback pre-wrap also covers chat/plan/FileViewer during their lazy window — that is the fix's intended scope per round-1 ("One rule fixes backlog, chat, plan, and FileViewer fallbacks") and matches the fallback's original documented intent ("preformatted text"). Strict improvement, not a regression.

## Finding 2 (low — chip mechanism deviation): RESOLVED

- **Flag added.** `frontend/src/lib/toolCardPaths.ts:174-177` — `md?: boolean` on `ToolCardChip`, with a doc comment scoping it to backlog_add's argLabel chip.
- **Push site sets it.** `frontend/src/components/chat/Message.tsx:699-705` — the argLabel chip push sets `md: name === "backlog_add"`. This is the only `md:` assignment in the codebase (repo-wide sweep), so no other chip can ever be md-flagged.
- **Render site is flag-driven.** `Message.tsx:801` — `{chip.md ? <InlineMarkdown text={chip.text} /> : chip.text}`; the old `name === "backlog_add"` render-site check is gone (the remaining `backlog_add` references in Message.tsx are the argLabel extraction at :593 and comments). Exactly plan step 4's prescribed mechanism.
- **No behavior change for other pathless chips.** Engine chips (`Message.tsx:718-723`, search/search_read only) and browser chips (`:736`) are pushed without `md` → plain text, unchanged. Other tools' argLabel chips get `md: false` (falsy) → plain text, unchanged. Overflow `+N` chips from `buildPathChips` carry no `md` → plain text, unchanged. The only observable delta vs. the round-1 code: a hypothetical future pathless chip on a backlog_add card now renders plain instead of silently as markdown — precisely the risk round-1 flagged, now closed.
- **Grouped calls.** The push sits inside the `for (const c of calls)` loop (`Message.tsx:692`) with `key: c.id`, so each grouped backlog_add call pushes its own md-flagged chip and each renders its own InlineMarkdown at the render site.
- **Tests pin the mechanism.** `frontend/src/components/chat/messageArgLabel.test.ts:283-290` — "scopes markdown rendering to backlog_add chips only" now asserts both `md: name === "backlog_add",` (push site) and `{chip.md ? <InlineMarkdown text={chip.text} /> : chip.text}` (render site); the old `name === "backlog_add" ? (` assertion is removed, so reverting to the name-check mechanism fails the test. The companion test (:278-281) still pins the InlineMarkdown import/usage.

## Regression sweep (no new issues)

- The rendered pipeline is untouched by the fixes: `MarkdownImpl` remarkPlugins wiring, BacklogView card/hover preview, editor textarea, steer bubbles — all unchanged in this diff (round-1 verified them; the fix diff touches none of them).
- `markdown-fallback` repo-wide: only `Markdown.tsx:23` (the div), the new test's expectation string, and the round-1 report text. No CSS rule needed (the utilities do the work); the retained class is a harmless semantic hook.
- TypeScript: `md` is optional on the interface, so all other `ToolCardChip` constructors (`buildPathChips`, engine/browser pushes, tests) compile unchanged — confirmed by the green tsc build.

## Remainder of the uncommitted set (benign)

- `.coding/backlog.jsonl` — one line: item c8531c34's `note` changes from null to commit hash `125838a3…` (the commit round-1 reviewed). Bookkeeping, not code.
- `.coding/reviews/af0a7ea2-backlog-markdown-pipeline-review.md` — the round-1 report itself (untracked), to be committed alongside this round-2 report per the closing sequence.

## Checks

- **Tests:** main agent's post-fix run reported green — frontend 764/764 across 53 files (+1 = the new fallback source-contract test), `npm run build` (tsc + vite) green, cargo test 1917 + 16 integration / 0 failed. Consistent with the diff: one new test, none removed or weakened.
- **Docs sync:** the fixes change no user-facing behavior descriptions (fallback styling and chip wiring are internal); the new/updated doc comments on `MarkdownFallback` and `ToolCardChip.md` satisfy the doc-comment style rule. README/PLAN.md were verified in round-1 and nothing here invalidates that.
- **Multi-platform:** frontend-only TSX/test changes; no platform-specific code or assumptions.
- **Code style:** matches existing conventions (source-contract test style, doc comments, comment voice); build is warning-free per the green runs.
