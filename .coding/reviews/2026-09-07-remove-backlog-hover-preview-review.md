## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on wt/agenticcoding for plan a9821f61 ("Remove the backlog hover preview — superseded by expanding details"). The removal itself is complete, surgical, and correctly re-pinned by tests; the coverage claim (no content unreachable) verifies against the source. Both findings are minor scope/polish issues, not correctness bugs: an undocumented className change on the card div (a beneficial typo fix that contradicts the "className kept" claim) and a comment misalignment in one edited test file.


### Verified — removal completeness (focus 1)
- Zero hover-preview machinery left in `BacklogView.tsx`: repo-wide search finds no `previewOpen` / `showPreviewDelayed` / `hidePreview` / `previewTimer` / `previewPos` / `showPreview` / `cardRef` / `createPortal` in the file (the only remaining frontend `createPortal` uses are the unrelated TraceStats tooltip, untouched). Full read of all 999 lines confirms no `onMouseEnter`/`onMouseLeave` anywhere — the only handlers are onClick/onChange/onKeyDown/onPaste/onDrop/onDragOver.
- The `createPortal` import is gone; `useState`/`useRef`/`useEffect`/`useCallback` all still have live uses (textareaRef, copyResetTimer, checkpointCopyTimer, editRef, editing, expanded) — consistent with the reported tsc exit 0.

### Verified — surgical removal (focus 2)
- KEPT and untouched: the copy-reset effect (:388-396), the expand/collapse machinery (hasBody :305, isLongBody :306, `useState(!isLongBody)` :307, chevron toggle :737-753 with `aria-expanded` + Expand/Collapse titles, expanded body render :801-805), the deferred checkbox + Deferred badge (:715-736), the editor, copy button, plan chip, note, and checkpoint detail. The diff touches only the preview block, the scroll-close effect, the card wiring, the portal block, the import, and the two doc comments.
- One deviation from "className kept" — Finding 1 below.

### Verified — coverage claim (focus 3)
No content becomes unreachable after the removal:
- **Headline** renders in BOTH branches (inside the chevron button :750-752 / plain div :757-759) with `break-words` and no truncate/line-clamp — a long single-line text with no body (hasBody=false → no chevron) still renders in full as the headline.
- **Body** is reachable via the chevron (collapsed by default only when >200 chars; short bodies start expanded).
- **Images**: the h-10 thumbnail strip renders whenever `!editing` regardless of expand state (:812) — images with no body stay visible.
- The old `showPreview` condition (text > 200 || images) vs the new reachability leaves no gap in any combination; the only loss is the popup's larger h-16 thumbnails — accepted and documented (focus 7).
- Observation (not a finding): the popup was the last surface rendering the *headline* through Markdown; the card renders the headline as plain semibold text — pre-existing headline-split behavior (backlog 40763a24), and the copy button + editor still expose the raw text.

### Verified — test quality (focus 4)
- The replaced test was necessarily replaced: it asserted `toContain("<Markdown remarkPlugins={[remarkBreaks]}>{item.text}</Markdown>")`, which would now FAIL (the only Markdown render is `{body}` :803).
- The removal pin (`not.toContain` previewOpen/showPreviewDelayed/hidePreview/createPortal) and the images-reachable pin (`{!editing && item.images.length > 0 && (`) correctly pin the new contract. The images-reachable string duplicates the pre-existing pin at :189 ("hides the static display thumbnails while editing") — harmless redundancy; both fail if the strip is removed.
- Expand coverage stays pinned (:111-119 — isLongBody, `useState(!isLongBody)`, setExpanded toggle, aria-expanded) and the body pipeline at :79-85.
- No test still references the hover preview as existing; the remaining "hover preview" text under `.coding/` is historical records (old plans/reviews/backlog items) — correctly left untouched.

### Verified — documentation sync (focus 5)
- README.md: "the card goes through the same pipeline as the plan display" — accurate.
- `markdownRendering.test.ts` :81-83 and the two `BacklogView.tsx` doc comments (:252-253, :302-303) — accurate.
- `messageArgLabel.test.ts` — content accurate, but the edit broke the comment's alignment — Finding 2.

### Verified — multi-platform neutrality + security (focus 6)
Pure frontend removal; no platform-specific code, no new APIs, no security surface. The removed portal was this file's only `document.body` usage. No concerns.

### Verified — accepted loss documented (focus 7)
The h-16 → h-10 thumbnail loss is noted in the new test's comment (BacklogView.test.ts :99-101: "the popup's larger h-16 view is the accepted loss; the user judged the expanding details sufficient") and in the plan resolution. Not silently dropped.

### Findings

**Finding 1 (LOW) — the card div's className was NOT kept verbatim: a typo fix snuck in (undocumented visual change).**
`BacklogView.tsx:553`. The plan (a9821f61.md, step 1) and the task description both say "keep the className", but collapsing the multi-attribute div to one line also changed `bg-bg-terriary` → `bg-bg-tertiary`. The old spelling was a no-op typo — the Tailwind theme defines `tertiary` (tailwind.config.ts:17) and every other usage in the codebase spells it correctly — so this is a real visual change: the card now gets the tertiary background fill (#334155 dark / #e2e8f0 light) where it previously had none. The fix is beneficial and matches the obvious original intent, but it is outside the plan's stated scope and undocumented. Action: accept it and correct the "className kept" claim in the plan/resolution (preferred — one less typo in the codebase), or revert to the exact previous className if the visual change is unwanted. Do not leave it unacknowledged in a "pure removal" change.

**Finding 2 (LOW) — comment misalignment in messageArgLabel.test.ts.**
:272-274. The three edited lines of the block doc comment now start with two spaces before the asterisk (`  * through InlineMarkdown…`) while the surrounding lines use the standard single space (` * …`), breaking the comment's alignment. Cosmetic; re-align to ` * `.

### Test status
Not re-run by the reviewer (read-only surface); the reported green matrix (tsc exit 0; vitest 75 files / 1046 tests; cargo lib 2079+16; src-tauri 245+4+2) is consistent with the source read — the replaced test's old assertion would have failed against the current source, and the new assertions match it. Rust is untouched (frontend-only diff).
