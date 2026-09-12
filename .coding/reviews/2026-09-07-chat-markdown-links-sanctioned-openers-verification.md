## Verdict: PASS

Verification pass for the four LOW findings from the prior review (`.coding/reviews/2026-09-07-chat-markdown-links-sanctioned-openers-review.md` — "FINDINGS (0 high, 4 low)") on commit **91cd60a** ("fix: chat markdown file links open through the sanctioned openers (plan 66a5ee24)", HEAD of `wt/agenticcoding`, clean working tree). All four fixes are correctly implemented, correctly ordered, and each carries a regression test that fails under the pre-fix code. No new findings.

## Scope verified

Commit 91cd60a in full (`git show`), cross-checked against the on-disk files (`git diff HEAD` and `git status` both empty — disk == commit):
- `frontend/src/lib/markdownLink.ts` — `normalizeLocalFileHref` (LOW 1/2/3)
- `frontend/src/components/chat/MarkdownLink.tsx` — `openMarkdownTarget` + `MarkdownLink` (LOW 4)
- `frontend/src/components/chat/MarkdownLink.test.tsx` — 17 tests incl. the four regression cases
- `frontend/src/components/chat/Message.tsx` — `a: MarkdownLink` wiring (unchanged from the reviewed changeset)
- `frontend/vitest.config.ts` — test registration; `README.md` + `.coding/` bookkeeping (spec, plan, backlog, prior report)

## Fix-by-fix verification (all four correct)

### LOW 1 — encoded literal `#` vs fragment separator: FIXED
`markdownLink.ts:33-42` — the fragment is stripped on the RAW href (`indexOf("#")` at :38, before `decodeURIComponent` at :48), so `%23` survives decoding as a literal hash. Traced `src/a%23b.md`: no raw `#` → no strip → decode → `src/a#b.md` → `SCHEME_RE` no match → returns `src/a#b.md`. The scheme check stays decode-then-check (:53-56), exactly as the prior review required — `%68ttps:`-style smuggling is still rejected. Regression test :67-71 (`src/a%23b.md` → `src/a#b.md`) fails under the old decode-then-strip code (which returned `src/a`).

### LOW 2 — query strings not stripped: FIXED
`markdownLink.ts:40-41` — the query is stripped at the first raw `?` in the same raw pass, after the fragment strip (so `?x=1#frag` strips both). Traced `src/foo.ts?x=1` → `src/foo.ts` and `src/foo.ts?x=1#frag` → `src/foo.ts`. Regression tests :73-77 pin both forms; both fail under the old code (query leaked into the opened path → viewer not-found).

### LOW 3 — UNC paths degraded to a bogus relative path: FIXED
`markdownLink.ts:58-64` — the `//` rejection now runs AFTER the backslash→forward-slash conversion (:59 convert, :64 `startsWith("//")` reject). Traced `\\server\share\x.md`: no scheme match (leading `\` fails `SCHEME_RE`) → convert → `//server/share/x.md` → rejected (null) → renders as plain text instead of a dead-end open to `server/share/x.md`. Forward-slash UNC (`//server/…`) is caught by the same check. Regression test :91 added to the rejects case; fails under the old pre-conversion check.

### LOW 4 — markdown link `title` dropped: FIXED
`MarkdownLink.tsx:51-58` — the props type now accepts `title?: string`; :69 merges it for external (`${title} (opens in your browser)`) and :85-89 for local (`${title} (opens ${localPath} in the Files tab)`), falling back to the fixed hint when absent. React escapes the interpolated title (no new XSS surface). Regression test :179-196 pins both merged forms; fails under the old props type (author's title absent from the rendered markup).

## Ordering + containment

The committed pipeline is exactly as claimed: trim → raw fragment strip → raw query strip → percent-decode (raw fallback on malformed) → scheme check on the DECODED form → backslash conversion → `//` rejection → `./` and `/` prefix strips → empty check. Blast radius confirmed by search: `normalizeLocalFileHref` is imported only by `MarkdownLink.tsx` + its test; `MarkdownLink` only by `Message.tsx` (:9, :310) + its test. Message.tsx's change remains additive (import + `a: MarkdownLink` + comment; the `code` override untouched).

## Nothing-broke verification

- Working tree clean — the reviewed disk state is exactly commit 91cd60a.
- `MarkdownLink.test.tsx` counted: 17 `it` blocks (8 normalizer + 4 router + 4 affordance + 1 source contract), matching the claim.
- Suite registration: `MarkdownLink.test.tsx` is in `vitest.config.ts`'s include list (:24), satisfying the vitestInclude guard. Include-list arithmetic: 67 individual entries + the settings glob expanding to 8 files = 75 files, matching the claimed "vitest 75 files / 1044 tests green, tsc exit 0" (attested in the commit message; not re-run by this read-only reviewer — but the structural preconditions all check out: registration present, the new tests use the established node-env harness patterns (renderToStaticMarkup, real `requestFileOpen` store action, `vi.mock` factory), and no other module consumes the changed code).
- The commit touches no Rust sources (frontend + docs + `.coding/` only) — "Rust suites untouched" is consistent with the diff.
- Doc sync: module doc comments updated to match the new semantics (`markdownLink.ts:22-27` documents the raw fragment/query strip; `MarkdownLink.tsx:47-49` documents the title merge). The README feature line remains accurate.

## Notes (no action required)

- A raw `?` in a filename (unencoded) is treated as the query separator and truncates — same semantics as a raw `#`; a literal `?`/`#` in a filename must be percent-encoded in markdown, which the doc comment states. Consistent, not a finding.
- The prior review's "no action required" notes (chat-only scope — FileViewer/PlanProgress/BacklogView links untouched; `openMarkdownTarget` re-normalizing the href) remain accurate and unchanged.
