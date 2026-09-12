## Verdict: FINDINGS (1 high, 1 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 534fa188 (Backlog tab: headline rendering + card polish, backlog 40763a24): `frontend/src/components/views/BacklogView.tsx`, `BacklogView.test.ts`, new `BacklogView.test.tsx`, plus `.coding/backlog.jsonl` and untracked `.coding/` sidecar files.

The implementation itself is correct and faithful to the plan — the one high finding is that the new test file never executes under `npm test`.

---

### HIGH 1 — New test file is not registered in vitest.config.ts: the regression tests never run

`npm test` is `vitest run` (frontend/package.json:11), and vitest resolves test files ONLY from the explicit `include` list in `frontend/vitest.config.ts` (lines 16–73). **`src/components/views/BacklogView.test.tsx` is absent from that list** — while its declared mirror `src/components/views/PlanStepRow.test.tsx` IS registered (line 36), and the older `BacklogView.test.ts` is registered (line 30).

Consequences:
- The plan's step-3 tests (headline semibold + chip, long-body collapse default, short-body expansion, headline-only no-toggle, all four `splitHeadline` edge cases) are dead code: `npm test` never executes them, so a future regression in the headline split goes undetected. The constitution's test rule and plan step 4 are not actually satisfied.
- The pre-review claim "vitest 63 files passed (incl. the new BacklogView.test.tsx)" cannot be true for `npm test`: the 63-file count matches the current config WITHOUT the new file (56 include entries, one being the `settings/**/*.test.ts` glob). The file was almost certainly never run.

Fix (one line): add `"src/components/views/BacklogView.test.tsx",` to the include list in `frontend/vitest.config.ts` (next to `PlanStepRow.test.tsx`, line 36), then re-run `npm test` in `frontend/` and confirm the file count grows by one and the new tests pass. I verified every assertion against the implementation, so they should pass as-is: `aria-expanded`/`rotate-90`/`font-semibold`/`bg-slate-700/50`/`uppercase tracking-wide` all match the JSX; the long-body fixture is ~268 chars (> 200 → collapsed); the headline-only card renders no button and contains no other `aria-expanded`; the `BacklogItem` fixture is type-complete (`deleted_at` optional).

### LOW 1 — Unrelated plan-171f8590 bookkeeping rides in this commit

The uncommitted tree contains, besides this plan's changes: (a) the `.coding/backlog.jsonl` flip of item `5b099aef` (per-model reasoning-effort) to `done`/`plan_id 171f8590`, and (b) the untracked knowledge file `.coding/knowledge/spec/2027-01-06-per-model-reasoning-effort-override-modelspec-re.md` — both belonging to plan 171f8590, whose code already landed in commit 86b1c21. The parent's task description ("Changed files (all frontend, no Rust changes)") does not mention them.

Not a code defect — but don't let them land silently mislabeled. Either sweep them consciously into the bookkeeping portion of the commit and say so in the commit message (the branch's established pattern, cf. ecee9c5 "bookkeeping: land web-fetch SSRF plan leftovers"), or commit them separately before this plan's commit.

---

## Verified correct (no action needed)

**splitHeadline logic (BacklogView.tsx:239–248)** — all edge cases correct and lossless:
- No newline → `{headline: text.trim(), body: ""}` (headline-only, no toggle).
- CRLF handled exactly once: `search(/\r?\n/)` finds the `\r`, `replace(/^\r?\n/, "")` strips the full CRLF from the body.
- Leading blank line → empty headline + body preserved (degenerate but lossless; pinned by test).
- Trailing newline / whitespace-only body → `hasBody` false → no toggle, no empty collapsible.
- React text-node rendering of the headline is escape-safe (no markdown injection surface).

**Display-only invariant (the core "no data loss" requirement)** — verified at every path:
- Copy: `navigator.clipboard.writeText(item.text)` (line 296) — full raw text.
- Editor: `editText` seeded/synced from `item.text` (lines 377, 384–389), saved via `backlogEdit(item.id, editText, editImages)` — raw text.
- Hover preview: renders `<Markdown …>{item.text}</Markdown>` (line 746) — raw text; its gate intentionally stays `item.text.length > 200 || item.images.length > 0` (line 315) as the see-everything escape hatch (per plan).

**No stale references** — `isLong`/`displayText` and the old click-to-expand `onClick`/`title` on the text div are fully removed; whole file read confirms no leftovers.

**PlanStepRow mirror (BacklogView.tsx:662–707 vs PlanStepRow.tsx:46–91)** — faithful: rotating `ChevronRight` (h-3.5 w-3.5 mt-0.5 text-slate-500 transition-transform, `rotate-90` when expanded), `aria-expanded` on the toggle button, spacer span (`mt-0.5 block h-3.5 w-3.5 shrink-0`) keeps body-less headlines aligned, semibold `text-slate-100` headline at `flex-1 min-w-0` with `break-words`. Collapse default `useState(!isLongBody)` with the 200-char threshold matching the old truncation threshold. Body renders through the existing markdown stack (backlog 991942af) only when expanded.

**Source-contract tests (BacklogView.test.ts)** — all updated pins match the new source exactly (verified line-by-line): `{body}` markdown wiring, `mt-2` note box, `isLongBody`/`useState(!isLongBody)`/`setExpanded((v) => !v)`/`aria-expanded={expanded}`.

**Constitution** — doc comments present on both new exports (`splitHeadline`, `BacklogItemCard`); existing code style followed; no `#[allow]`-style suppressions; regression coverage added (pending the HIGH 1 registration fix).

**Multi-platform neutrality** — pure React/TSX, no platform-specific code, no Windows-only assumptions.

**Documentation sync** — README.md has no Backlog-tab section; PLAN.md's "Backlog + Run-All" bullet (lines 46–81) documents the subsystem at the feature/lifecycle level, not card rendering — a UI-polish change needs no doc update. Module doc comments are in place.

**Minor observations (per plan spec, not findings)**: the headline renders as plain text (no markdown) — intentional, documented in the test file header; `expanded` does not recompute if an item's text is later edited across the 200-char class boundary (the plan pinned exactly `useState(!isLongBody)`; the chevron remains available); body color slate-200 → slate-300 is part of the headline-emphasis polish.
