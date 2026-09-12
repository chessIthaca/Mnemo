# Code Review — Endpoints tab: scroll fix + collapsible endpoint cards

**Reviewer:** read-only reviewer (spawn_agent)
**Date:** 2026-04-04
**Scope:** all uncommitted changes (`git status` / `git diff HEAD`):
- `frontend/src/components/layout/ConfigDialog.tsx` — the feature under review
- `.coding/plans/d4af0f52-9e67-479c-8512-19940c8b3b1f.md` — leftover plan checkbox flip (explicitly out of scope, not flagged)
- `.coding/plans/stack.json` — bookkeeping (explicitly out of scope, not flagged)
- `.coding/plans/01abd95d-6a6e-4cb4-b4bc-68994d7b8072.md` — untracked new plan file (bookkeeping, not flagged)

---

## Verdict

No correctness, security, or constitution-compliance findings. Two minor cosmetic/a11y nits (below). The scroll fix is correct and the collapse logic is sound.

## What was verified

### 1. Scroll fix (correctness) — OK
The flex chain is now properly constrained end-to-end:
- `DialogContent` (L76): `flex max-h-[90vh] flex-col overflow-hidden`
- Body div (L96): `min-h-0 flex-1 overflow-hidden` — new
- `Tabs` (L97): `flex h-full flex-col`
- Appearance `TabsContent` (L116): `min-h-0 flex-1 space-y-5 overflow-y-auto` — new
- Endpoints `TabsContent` (L326): `min-h-0 flex-1 overflow-y-auto` — new

`min-h-0` overrides the flex item's `min-height: auto`, so both panels can now shrink below their content height and the inner `overflow-y-auto` engages; the outer `overflow-hidden` no longer clips. Both tabs are covered (no remaining trap in the appearance tab). The `flex-1` basis of 0% lets the active Radix panel fill the area below the fixed-height `TabsList`. Correct.

### 2. Collapse logic — OK
- **All collapsed on load:** `expanded` initialized `new Set()` (L464) and reset in `load()` via `setExpanded(new Set())` (L499); `load()` runs on mount (L511-514) and on Retry (L640), and the dialog unmounts `EndpointsTab` on close (Radix Dialog), so every open/reload starts collapsed. ✓
- **Auto-expand on add:** `addEndpoint()` (L536-542) mints one `uid`, appends it to `uids`, and adds it to `expanded` via an immutable functional update (`new Set(s).add(uid)`) — no stale-closure risk. ✓
- **Delete prunes:** `deleteEndpoint()` (L552-556) removes `uids[i]` from the set in a functional update; deleting a collapsed card whose uid isn't in the set is a safe no-op. ✓
- **Toggle flips only the intended card:** `onToggle` (L665-674) reads `uids[i]` and flips exactly that uid with an immutable functional update. ✓
- **Fallback keys:** `expanded.has(uids[i] ?? 'ep-fallback-…')` — fallback keys are never in the set, so a row with no uid renders collapsed, and `onToggle` early-returns without a uid. Intentional and consistent with the React `key` (L659). ✓
- **Rename:** `onNameChange` re-keys `apiKeys` and `defaultProvider` under the new name (L686-699) — pre-existing logic, unchanged by this diff, no regression. The `expanded` set is keyed by uid, not name, so renames don't disturb it. ✓
- **Uid uniqueness:** `makeUid()` uses `Math.random` (L424) — collision practically impossible; pre-existing, unchanged.

### 3. Collapsible UX / a11y — OK (two nits)
- Chevron button (L810-818) has `aria-expanded`, `aria-label` ("Expand/Collapse endpoint"), and `title`; chevron rotates via `transition-transform` (`-rotate-90` when collapsed, `ChevronDown` when open). ✓
- The summary-row button (L848-860) is a **sibling** of the header row, not nested inside any button; it has visible text content (kind badge, truncated base_url, model count) so it's accessible by content. ✓
- The name input (L823-830) stays rendered and editable while collapsed — only the detail fields are wrapped in `{open && (<>…</>)}` (L863-1011). ✓
- Delete button and "Default" badge remain visible while collapsed. ✓
- No duplicate HTML ids introduced: the detail-field ids (`ep-kind-`/`ep-url-`/`ep-key-`/`ep-effort-` + name) are unmounted while collapsed and are pre-existing patterns; same-name id collisions (e.g. two blank "New endpoint" cards) are pre-existing.
- `showKey` per-card state persists across collapse/expand because the card stays mounted (keyed by uid) — only children unmount. ✓

### 4. Security — OK
- The API key field (L894-915) is inside the `{open && …}` block, so it's not in the DOM at all while collapsed — a slight reduction in exposure vs. the previous always-rendered password field. The collapsed summary shows only kind / base_url / model count — no key material, no key in `title`/`aria-label`. No new leakage.

### 5. Constitution compliance — OK
- No commits made (all changes uncommitted); current branch is `feat/bookkeeping-tools-autorun`, not main. ✓
- Line endings: byte-level scan of `ConfigDialog.tsx` — 1014/1014 line endings are CRLF, 0 bare LF, 0 stray CR, 0 trailing-whitespace lines. The PowerShell re-indent did not corrupt endings, and `git diff` shows hunks only (no whole-file rewrite). ✓
- New props (`open`, `onToggle`) are documented with a doc comment (L784-786). ✓
- No build output in the working tree. ✓

## Findings

### Minor (cosmetic)
1. **Inconsistent indentation inside the `{open && (<>…</>)}` block** — `ConfigDialog.tsx` L863-1011. The first inner block (kind/url grid, L865-892) sits at 8/12/10 spaces while the sibling blocks (L894+) sit at 10/12 — L865 (`<div className="grid grid-cols-[7rem_1fr] gap-2">`) is out of line with its siblings. Harmless (JSX ignores indentation) but a Prettier pass over the block would normalize it.

### Minor (a11y nit)
2. **The summary-row expand button lacks `aria-expanded`** — `ConfigDialog.tsx` L848-860. It only renders in the collapsed state so its state is implied, but adding `aria-expanded="false"` would make the duplicate toggle control consistent with the chevron button (L812). Not a regression; the chevron control itself is fully labelled.

## Conclusion

No findings that require code changes for correctness, security, or constitution compliance. The two nits are optional polish. The plan-file and stack.json changes are bookkeeping, per the review brief.
