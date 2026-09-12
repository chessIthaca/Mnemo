## Verdict: FINDINGS (0 high, 4 low)

Review of ALL uncommitted changes (git diff HEAD + untracked) for backlog 900fe8d8 / plan 10650092 — frameless + normal-size treatment extended to the memory/vision/skill activity cards. Method: full line-by-line read of every changed region in `Message.tsx` (all five card scopes: MemoryEntryCard :120-215, VisionEntryCard :230-306, skill case :392-410, ToolCard :664-871, CallDetail :873-1005) and the new test block (`toolCardPaths.test.ts` :735-859), plus the font-contract anchors (Conversation.tsx :88-100, globals.css :8/:234), the DECISION/SPEC records, README/PLAN doc-sync claims, and CR/BOM scans of both PowerShell-spliced files. Test evidence (vitest 768/768 incl. 4 new, tsc+vite green, cargo test 1938/0) accepted as reported; no Rust changes.

**The conversion itself is correct and complete — no information lost, no broken affordances, no JSX damage.** All four findings are low-severity claim-accuracy / contract-pinning gaps, each a small fix.

## Verified correct

1. **Affordances survive everywhere.** MemoryEntryCard: chevrons (`h-[1em] w-[1em]`), `role="button"` + `tabIndex` + Enter/Space handler with `preventDefault`, 🧠 + name + query chip + tier badge (`px-[0.375em] py-[0.125em] text-[0.65em]`) + title + snippet + matched count + ✓/✗, expanded hit rows (tier/title/score). VisionEntryCard: same expand interaction, 🖼️ + index/total + thinking-dots + description + ✓/✗, expanded query/response sections with `text-[0.75em]` labels and 1em content. Skill case: `▶ skill "name" start` + prompt subtitle. ToolCard: header `role="button"` span + keyboard handler, chevrons, displayName, file-link chips (`stopPropagation` + `openDiffInViewer`/`openFileInViewer` + titles), md/engine/browser chips, running dots + `text-[0.75em]` status, ✓/✗ ×N, searchNotes, one-line error summary, expanded CallDetail list, inline-images row (`gap-[0.5em]`). CallDetail: labels/pre blocks keep `text-[0.75em]`, readSections rows clickable, UnifiedDiffView intact.
2. **Font contract holds in the changed code.** Conversation.tsx:94 `fontSize: "var(--app-font-size)"` and globals.css:234 `.prose { font-size: inherit; }` verified. No `text-xs`/`text-sm`/`text-[0.875em]` on any card container (all five scopes read); ToolCard header, searchNotes, and error summary now inherit 1em; status/running/labels keep the em-based `text-[0.75em]`/`text-[0.65em]` treatment. The only rem-based utilities in the card fns are pre-existing and unchanged (see Finding 1).
3. **caseSrc/fnSrc scoping is correct.** `lastIndexOf('case "skill":')` picks MessageImpl's (:392); the only other occurrence is arePropsEqual's (:67, verified), and no later one exists (displayName's switch :648-661 has no skill case; argLabel uses if-chains). The `\n    case ` terminator lands on `case "qa":` (:411) — exactly the render block. All four `function NAME(` targets are unique; CallDetail is the last fn in the file so its slice-to-EOF includes nothing else. Both helpers fail loud on structural drift.
4. **Doc sync.** README's card mentions (:53, :67, :68, :75, :76) are feature-level — no styling claims; PLAN.md:771 `ToolCallCard` is a historical Phase C inventory. Agree: no updates needed. MemoryEntryCard/VisionEntryCard doc comments carry the new frameless rationale; the DECISION/SPEC records match the code (with the overstatements in Findings 1/3).
5. **Multi-platform neutrality.** Pure className/TSX changes; the test uses `node:fs` per the established node-env suite pattern. No platform assumptions.
6. **Splice integrity.** Zero `\r` in either file; git diff seams clean (no line-1 changes → no BOM, no wholesale line-ending rewrite); the new describe sits exactly between the "expandable memory_search card" describe (ends :733) and the fileEditDiff doc comment (:861); both files read as well-formed TS/TSX end to end.
7. **Out-of-scope residuals** — agree with all documented decisions (user/error/steer/qa frames are message-level or interactive UI, not activity cards; streaming `text-sm` :337 is assistant text, pre-existing; InlineMarkdown/CodeBlock untouched), except the transcript gap (Finding 4).

## Findings

### F1 (low) — `max-h-64` in CallDetail falsifies the "zero rem-based utilities in the card fns" claim

`Message.tsx` :983, :990, :997 — the three scrollable pre blocks (`max-h-64` = 16rem) are rem-based and sit inside CallDetail, one of the audited card fns. Pre-existing and untouched by this diff, but the plan goal ("zero rem-based utilities remain in the card components"), the DECISION record ("All card utilities are em-based per SPEC 4c2f16ba"), and the review task's own claim are all literally false as stated. `max-h-64` is also absent from `REM_UTILITIES`, so the audit cannot catch a re-added one. The two-font-size guarantee itself isn't broken (text size is correct; only the scroll cap stays 256px, so large `--app-font-size` shows fewer lines before scrolling) — hence low.
**Fix:** either convert to an em-based cap (e.g. `max-h-[16em]`–`max-h-[18em]`, keeping the default-size appearance roughly constant while scaling with the setting) and optionally add `"max-h-64"` to `REM_UTILITIES`, or keep it and narrow the records' claim to "spacing/font utilities" with the height-cap exception documented.

### F2 (low) — `REM_UTILITIES` misses `text-sm`, which the records explicitly forbid

The SPEC record and DECISION both say "no text-xs/**text-sm**/text-[0.875em] downscale on card containers", but the denylist (`toolCardPaths.test.ts` :773-788) pins only `text-xs`, `text-[0.875em]`, and `text-[0.65rem]`. A re-added `text-sm` on a card container would pass the audit. (The list is generally a curated denylist of the old code's values — `max-h-64`, `p-2`, `gap-4`, `text-base`, `h-4` etc. would also slip through; `text-sm` is the one named in the contract, so pin it.)
**Fix:** add `"text-sm"` to `REM_UTILITIES`. No false positive today — verified none of the five scopes contains that substring (`text-slate-*` doesn't match).

### F3 (low) — ToolCard's frameless state isn't pinned, though the DECISION claims it is

The frame-class loop (no `rounded-lg`/`border-border`/`bg-bg-tertiary/50`/violet frames, `py-[0.125em]` outer) scopes only MemoryEntryCard, VisionEntryCard, and the skill case (:792-796). The ToolCard test (:820-833) pins only the `text-[0.875em]` absence and `REM_UTILITIES` — a re-added frame on ToolCard would not be caught. The DECISION record states the block pins "fn-scoped: no frame classes, no rem utilities, py-[0.125em] outer, affordances intact", which overstates the ToolCard coverage.
**Fix:** add `["ToolCard", fnSrc(src, "ToolCard")]` to the `scopes` array — it passes today (ToolCard contains none of the frame strings and does contain `py-[0.125em]`, verified), and the DECISION's claim becomes true. (Optionally also pin the VisionEntryCard affordances the way the memory card's are in the fourth test.)

### F4 (low) — transcript stack gap `space-y-2` vs SPEC 4c2f16ba's "chat transcript" clause

`Conversation.tsx:97` — `space-y-2` (0.5rem) is the inter-card gap of exactly these cards, and SPEC 4c2f16ba reads "Tool-card (and chat transcript) spacing must use em-based Tailwind arbitrary values … NOT rem-based utilities". Pre-existing and outside the card components (hence documented as out of scope), but the DECISION record re-asserts the rem value ("Inter-card gap is the transcript stack space-y-2"), leaving a live SPEC-vs-code tension this change is the natural place to resolve.
**Fix:** one-line conversion `space-y-2` → `space-y-[0.5em]` (identical at the default 14px setting, scales thereafter), or record an explicit exception in SPEC 4c2f16ba / the DECISION. Either resolves the tension; converting is the cheaper and more consistent option.

## Notes (no action required)

- `rounded` (0.25rem border-radius) on CallDetail's pre blocks and the tier badges' `bg-violet-900/50` chip are label/code treatments, not card frames — consistent with the DECISION's carve-out; the frameless rule is about the outer div, and the test's `rounded-lg` assertion correctly doesn't flag bare `rounded`.
- `underline-offset-2` in ToolCard (:794) is px-based (not rem) and purely cosmetic — fine.
- `text-[0.875em]` sitting inside `REM_UTILITIES` despite being em-based is intentional (it pins the removed header downscale, not a rem value) — the list name is slightly loose but the doc comment at :770-772 explains the intent.
- Streaming assistant text (`text-sm`, :337) vs finalized markdown (`.prose` inheriting `--app-font-size`): at non-default font sizes the streaming text renders at a fixed 14px until finalization — pre-existing, assistant text (not a card), documented out of scope. Worth a backlog item someday, not a finding here.
