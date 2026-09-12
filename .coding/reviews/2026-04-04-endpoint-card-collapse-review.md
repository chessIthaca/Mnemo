# Code Review — EndpointCard collapsed single-row render

**Reviewer:** read-only reviewer subagent
**Date:** 2026-04-04
**Scope:** ALL uncommitted changes (`git diff HEAD`): `frontend/src/components/settings/sections/EndpointCard.tsx` (the planned change), plus `src-tauri/src/ipc/events.rs` (out of stated scope) and `.coding/plans/*` bookkeeping.

## Plan goal

Settings → Providers endpoint cards previously rendered two rows even when collapsed. The change wraps the inner kind/URL/model-count box in `{open && ...}`, adds a muted `{N} model(s)` span to the header row rendered only when `!open`, and re-indents the wrapped block.

## Correctness — no findings

- The two count renders are mutually exclusive: header span is `{!open && ...}` (EndpointCard.tsx:225–229), the gray box with its own count button is `{open && ...}` (:241–280). No duplication when open, none missing when collapsed.
- Pluralization `length === 1 ? "" : "s"` is correct for 0 ("0 models"), 1 ("1 model"), N.
- The new span is a `shrink-0` flex sibling between the Default badge and the delete button; no structural interaction with either (verified full header row, :189–239).
- The gray box's "N models" button (which also calls `onToggle`) is now unreachable when collapsed. Acceptable: the header chevron (:190–201) is always rendered and remains the toggle. When expanded, that button only ever collapses, so its pre-existing `aria-expanded={open}` attribute is now constant-true — harmless redundancy, not a regression.
- Existing effects still make sense: `showKey` reset on `!open` (:82–84) remains necessary because the API-key row is inside the expanded details block; the picker outside-mousedown effect (:155–165) is unchanged.
- sr-only labels and their `htmlFor`/id pairs for kind/URL unmount together inside the same conditional — no dangling ARIA references when collapsed.
- Re-indentation is a uniform 2-space shift matching the new nesting depth; JSX structure is well-formed.

## Bugs — no blocking findings (two informational notes)

1. **(Pre-existing, not introduced by this diff)** Collapsing a card while a model picker is open leaves `pickerOpen`/`pickerQuery` set and the document `mousedown` listener attached (:155–165); the next click anywhere calls `closePicker(true)` and can commit a stale `pickerQuery` via `onModelChange` while the row input is unmounted. The models block was already `{open && ...}` before this change, so this diff neither causes nor worsens it. Optional future hardening: also reset `pickerOpen` in the existing `if (!open)` effect at :82–84. No fix required for this change.
2. **(Nit)** The collapsed header row gains one more `shrink-0` element; the `flex-1` name input has no `min-w-0`, so on very narrow containers a long endpoint name plus badge + count + trash could overflow the card width. Pre-existing flex pattern; cosmetic only in a fixed-width settings dialog.

## Security — no findings

Pure conditional rendering; the count is a number escaped by React. No injection surface.

## Constitution compliance — no findings

- No `#[allow(...)]` added anywhere in the diff; no warning suppressions of any kind.
- Doc comments intact: `EndpointCard` keeps its `/** */` block (EndpointCard.tsx:22–25); `ProvidersSection` keeps its block (ProvidersSection.tsx:22–26, file unchanged in this diff).
- Line endings: no `^M` or mixed CRLF/LF in the EndpointCard.tsx diff. The single git warning ("LF will be replaced by CRLF") concerns `.coding/plans/b7fa9268-….md` — generated plan bookkeeping, not a hand-edited source file.
- Warning-free build under `#![deny(warnings)]` and green `npm run build` / `npm test` / `cargo test` are as reported by the main agent; I am read-only and cannot re-run builds.

## Scope note (not a defect)

The working tree also contains `src-tauri/src/ipc/events.rs:188`: `WorkflowStateChanged { state }` → `{ state, .. }`. The variant (`src/runtime/channels.rs:257–265`) has two fields (`state`, `top_plan_id`); a struct-variant pattern that omits a field without `..` is E0027 (hard compile error), so this hunk is behavior-preserving (the block only reads `state`) and was required for the tree to compile. Recommend mentioning it in the commit message, since the plan task described only the EndpointCard change. The `.coding/plans/*` modifications are normal workflow bookkeeping.

## Verdict

**No blocking findings.** The diff correctly implements collapsed single-row cards with no count duplication, no toggle regression, and full constitution compliance.
