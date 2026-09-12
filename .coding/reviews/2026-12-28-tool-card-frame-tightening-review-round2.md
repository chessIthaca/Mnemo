## Verdict: PASS

Round-2 re-review on `wt/agenticcoding` for plan 3de07a3b "Tighten tool-card display: frameless, tighter spacing, keep affordances", verifying the single round-1 finding (L1) is fixed and the rest of round 1's clean bill still holds. Method: full `git diff HEAD` + `git status`, direct read of the current ToolCard region (Message.tsx 725–851), repo-wide search for `py-0.5` remnants across frontend TSX, and read of the new em-based spec file. Read-only reviewer — suites not re-run; the reported green matrix (frontend vitest exit 0, `npm run build` = tsc + vite exit 0, cargo test 1898 passed / 0 failed) is statically consistent with the change set (frontend-only className edits since round 1, no test files touched, no Rust changes).

### L1 fix — verified

- **Em-based value.** Message.tsx:738 outer div is now `<div className="py-[0.125em]">` — em-based, so the card's vertical padding scales with `var(--app-font-size)` set on the Conversation container, restoring the convention established by plan cf18d69a. The round-1 state (`py-0.5`, rem-based) is gone.
- **Correct proportion.** The removed frame padding was `p-[0.5em]` (all sides); `0.125em = 0.5em ÷ 4` — exactly the intended 4×-tighter vertical padding, with horizontal padding dropped entirely (consistent with the frameless look).
- **Nothing else changed since round 1.** Every other hunk in the diff matches round 1's verified-clean descriptions verbatim: borderColor/bgColor deletion + comment update, header `gap-[0.4em]`→`gap-[0.25em]`, search-notes and error-summary `mt-[0.3em]`→`mt-[0.15em]`, expanded detail `mt-[0.5em] space-y-[0.75em]`→`mt-[0.25em] space-y-[0.4em]`, inline images `mt-[0.4em]`→`mt-[0.2em]`, Conversation.tsx `space-y-4`→`space-y-2`. The only delta is the one token `py-0.5` → `py-[0.125em]`.

### Round-1 clean bill — re-confirmed on the current tree

- **Frame removal + dead-code hygiene.** Outer div (Message.tsx:738) carries no `rounded-lg`, `border`, `${borderColor}`, or `${bgColor}`; the `borderColor`/`bgColor` declarations are fully deleted with the comment updated to "Header color by state" (line 730); `textColor` kept and still consumed by the header span (line 751).
- **Every keep-list affordance intact** (all confirmed in the current file, none degraded): expand/collapse via `role="button"` span with `tabIndex`, `onClick`, and Enter/Space `onKeyDown` (741–750); file-path chips with deep links — `file_edit` → Diff tab (`openDiffInViewer`), all others → Files tab with line anchor (755–789); +N overflow chip (buildPathChips / toolCardPaths.ts untouched); engine/browser info chips (unchanged region); running dots + "running" label (790–798); ✓/✗ ×N (799–804); search notes (806–814); one-line error summary (815–823); expanded CallDetail (824–836); inline image thumbnails (842–848).
- **CodeBlock.tsx and ToolImage.tsx untouched** — git status shows only the 4 expected modified files (.coding/backlog.jsonl, the superseded spec f5.md, Conversation.tsx, Message.tsx) plus untracked .coding files; the copy affordances living in those child components are safe.
- **.coding bookkeeping unchanged since round 1.** backlog.jsonl (a8020e04 pending→failed with an honest note, deee3b8f in_flight) and the f5 spec's `status = "superseded"` marker are exactly what round 1 sanity-checked.

### No new issues

- **Tailwind validity.** `py-[0.125em]` uses the same arbitrary-em syntax already proven throughout the file (`gap-[0.25em]`, `mt-[0.15em]`, …); tsc green confirms no unused locals.
- **Convention codified.** The new untracked spec `.coding/knowledge/spec/2026-12-28-tool-card-spacing-must-be-em-based-scales-with-a.md` accurately records the em-based rule, cites cf18d69a as its origin and this L1 finding + fix as the re-violation/repair — proper hygiene that prevents recurrence.
- **`py-0.5` remnants.** Repo-wide frontend search finds 62 `py-0.5` uses in 23 files, none in the ToolCard region; the Message.tsx hits (lines 150/196/349) are pre-existing memory-badge/inline-code styling outside this diff and outside the tool-card convention's scope — pre-existing, not introduced here, not a finding.
- **Multi-platform neutrality / security.** Frontend-only className strings; no platform-specific code, paths, or shell syntax; no new event handlers, no URL/href construction, no data-contract changes.
- **State scannability** unchanged from round 1's assessment: running = yellow-400 header + animated `thinking-dots` + literal "running" text; failed = red-400 header + ✗ + red one-line error summary; done = slate + ✓ — redundant color+animation+text+symbol signaling without the border.

No findings. The L1 fix is exactly the one-token repair round 1 prescribed, all affordance and hygiene guarantees from round 1 hold on the current tree, and the reported test matrix is consistent with the change set.
