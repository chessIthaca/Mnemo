# Review: Vision picker — stop hiding non-flagged models

**Scope:** `git diff HEAD` — `ModelPickerDropdown.tsx`, `VisionSection.tsx`
(plus trivial `.coding/plans/*` checkbox/stack changes, ignored as instructed).

## Summary

The fix replaces the old "hide non-capable models when any model is flagged
capable" logic with "show all models, sort capable-first, annotate with a
badge". Verified correct and backward-compatible.

## Correctness

- **Sort puts capable first** — `VisionSection.tsx:139-141`:
  `[...visionModels].sort((a,b) => Number(b.vision_capable) - Number(a.vision_capable))`.
  `vision_capable` is `boolean`; `Number(true)=1`, `Number(false)=0`, so
  capable (`1`) sorts before non-capable (`0`). Correct. `Array.prototype.sort`
  is stable, so relative order within each group is preserved.
- **`capableIds` derivation + pass-through** — `VisionSection.tsx:135`
  `new Set(capable.map(m => m.id))`, passed at `:289`. Correct.
- **Note text branches** — `VisionSection.tsx:295-299`:
  - `capable.length > 0` → "N of M model(s) are vision-capable (shown first)". Correct.
  - else `visionModels.length > 0` → "No models reported as vision-capable — pick your vision model". Correct.
  - else → "No models available". Correct.
  All three branches reachable and mutually exclusive. Correct.
- **No model hidden** — `pickerModels` now maps over ALL `visionModels`, never
  filters by capability. The user's vision model is always present (subject
  only to the `pickerQuery` text filter, which is pre-existing behavior). The
  reported bug is fixed.

## Bugs

- **No dangling references** — `modalityKnown` removed from `VisionSection.tsx`;
  grep across `**/*.{ts,tsx,rs}` returns zero matches. No dead code left.
- **`visionCapableIds` defaults safely** — declared `visionCapableIds?: Set<string>`
  (`ModelPickerDropdown.tsx:33`), accessed via optional chaining
  `visionCapableIds?.has(m)` (`:111`). When omitted (both `EndpointCard.tsx`
  callers at `:471` and `:538` do not pass it), it is `undefined`, the `?.has`
  short-circuits to `undefined` (falsy), no badge renders. No runtime error.
  Backward-compatible. Correct.
- **TypeScript** — prop type `Set<string>` matches the `new Set(capable.map(m => m.id))`
  value (`string[]` → `Set<string>`). No type issues.

## Security

- No backend changes; the `list_vision_models` IPC command is untouched.
- No API key handling changed. Error messages flow through the existing
  `String(e)` path (`VisionSection.tsx:97`) — no new key-leak surface introduced.

## Constitution compliance

- **Windows project** — no Linux paths or bash syntax in the diff (frontend TSX only).
- **Doc comments** — the new `visionCapableIds` prop has a JSDoc
  (`ModelPickerDropdown.tsx:29-32`). Existing public functions retain docs.
- **Line endings** — no mixed `\r\r\n` / mixed endings detected in the changed
  source files. (Git warns about LF→CRLF normalization on the `.coding/plans/*.md`
  bookkeeping files, which is expected and unrelated to the source change.)

## Findings

**No findings.** The diff is clean and correctly implements the intended fix.
