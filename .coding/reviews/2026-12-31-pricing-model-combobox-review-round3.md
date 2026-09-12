## Verdict: FINDINGS (0 high, 1 low)

Round-3 verification of plan ec52b07f ("Pricing model field: editable combobox of configured models", backlog 82dd66fc) at commit a96ab1c — HEAD of `wt/agenticcoding`, parent 4e9641e (both confirmed via git log), tree clean (`git diff HEAD` and `git status --short` both empty), round-2 report included in the commit. Both round-2 findings are resolved exactly as specified: F1's two mousedown guards plus the activeElement-guarded refocus are in place, and F2's restructure moves the empty-filter message outside the listbox with a never-dangling aria-controls. The full interaction matrix traces clean on both platforms for every primary flow, and the skipOpenRef flag has no stale path. One new LOW finding (F3): the toggle's preventDefault means a dropdown opened via the chevron while focus is outside the combobox root is no longer armed for the blur-close on Windows (pre-fix, the chevron's mousedown pulled focus into the root), so focusing another row's field can leave two dropdowns open at once — an edge case against L3's "at most one dropdown open" contract, cosmetic and recoverable. Test evidence is statically consistent (13 + 8 + 4 new tests counted in the files; 789 + 22 + 3 = 814).

### Round-2 fix verification (against the actual sources at a96ab1c)

**F1 (high) — RESOLVED.** The toggle button carries `onMouseDown={(e) => e.preventDefault()}` (ModelCombobox.tsx:110) and every entry button carries the same guard (:133) — exactly two occurrences in the file, matching the new count-2 contract test. The L2 refocus is now wrapped in `if (document.activeElement !== inputRef.current) { skipOpenRef.current = true; inputRef.current?.focus(); }` (:148-151), exactly the trap-avoidance shape the round-2 report prescribed, with the round-1 contract strings (`skipOpenRef.current = true;` :149, `inputRef.current?.focus();` :150) intact verbatim. The doc comment (:17-28) was updated to rounds 1-2 and accurately describes both the WebKit mechanism and the guard.

**F2 (low) — RESOLVED.** The positioned panel div (:120) no longer carries id/role; it renders either the empty-filter message div (:121-124) or the `id={listId} role="listbox"` div (:126) whose only children are the mapped `role="option"` buttons (:127-162). The input's `aria-controls={open && visible.length > 0 ? listId : undefined}` (:90) points at the listbox in exactly the state where it renders — never a dangling reference.

### Interaction matrix (traced on Windows/WebView2 — buttons focus on mousedown — and macOS/WKWebView — they do not)

1. **Mouse entry select** (field focused, list open — the primary flow): the guard suppresses the mousedown focus change, so focus stays on the field on BOTH platforms → no blur, no premature unmount → the click dispatches on the still-mounted entry → `onChange(o.model)` + `setOpen(false)`; `document.activeElement === inputRef.current` (focus never moved) → the refocus guard is false → no flag set, no `focus()` call. Selection works and the list closes. ✓ / ✓
2. **Keyboard Tab → Enter/Space select**: Tab moves focus field → first entry (the toggle is `tabIndex={-1}`); the blur's relatedTarget is contained → no close. Enter/Space fires the entry's click → `onChange` + `setOpen(false)`; activeElement = the entry button ≠ input → guard true → flag set → `focus()` fires the focus event synchronously → onFocus consumes the flag and returns before `setOpen(true)`; the subsequent re-render unmounts the entry with focus already safe on the field. ✓ / ✓ (keyboard focus is platform-uniform; preventDefault on mousedown does not affect key activation)
3. **Toggle click** (field focused): the guard keeps focus on the field → no blur → a single click toggles open/closed. The round-2 macOS double-click symptom is gone. ✓ / ✓
4. **Typing with the list open**: no focus change; the filter recomputes per keystroke. Filter to empty → message div renders, `aria-controls` becomes undefined; filter back to non-empty → listbox returns with `aria-controls={listId}`. ✓ / ✓
5. **Tab out**: field → entry (contained, list stays open) → next focusable outside the root → uncontained blur → close. ✓ / ✓
6. **Click into another row's field** (this field focused): text inputs take focus on mousedown on both platforms → this row's blur carries an uncontained relatedTarget → close; the other row's onFocus opens its list. At most one open. ✓ / ✓ — caveat: this holds only while this root contains focus; see F3 for the chevron-opened case.
7. **Overlay click**: mousedown on the non-focusable overlay moves focus to body → uncontained blur → close (the overlay's own onClick closes too). ✓ / ✓

**skipOpenRef can never go stale.** The flag has exactly one write site (:149), inside the `document.activeElement !== inputRef.current` check (:148), immediately before `inputRef.current?.focus()` (:150), and is consumed synchronously in onFocus (:97-100). Staleness would require `focus()` to fire no focus event — excluded on both counts: (a) the guard's operand cannot change between check and call (synchronous code, nothing intervenes), and (b) `inputRef.current` is non-null whenever an entry's onClick can run (the input is unconditionally rendered). The keyboard path engages the guard (activeElement = the entry button) and consumes it; the mouse path never sets it (focus never left the field). The round-2 trap is correctly avoided.

### Findings

#### F3 (low) — a chevron-opened dropdown with focus outside the root is not armed for the blur-close (two dropdowns can be open at once)

Mechanism, step by step (verified against the source):

1. Pre-fix on Windows, clicking the chevron focused the toggle button on mousedown (Chromium/WebView2 focuses buttons on mousedown) — focus entered the root, arming the L3 blur-close. The F1 guard suppresses exactly that focus change (its purpose on macOS), so on BOTH platforms a chevron-opened dropdown can now be open while focus sits outside the root — e.g. on the same row's cost input, another dialog control, or body (fresh dialog, chevron clicked as the first interaction).
2. React's onBlur on the root (:68-73, :76) fires only for focusout events originating from descendants of the root. With focus already outside, subsequently clicking another row's model field moves focus directly into that row — no focusout ever originates inside this root, so the blur-close never fires.
3. The other row's onFocus (:96-102) opens its dropdown → two dropdowns open simultaneously — the situation L3 was added to prevent ("at most one dropdown open across the pricing rows").
4. Recoverable but awkward: clicking the stale row's field does not close it (onFocus only opens — `setOpen(true)` is a no-op on an already-open list); the stale row's chevron or an entry select does close it.

Reachability: focus outside the root → chevron click → focus another row's model field (e.g. edit a row's cost, browse that row's models via the chevron, then click the next row's model field). Platform history: NEW on Windows — pre-fix, the chevron's mousedown pulled focus into the root, so the blur-close was always armed there; pre-existing on macOS (WebKit never focused the toggle; round 2's "cross-row close still works" verification implicitly assumed the field-focused primary flow, which is the only flow where it holds). Severity LOW: secondary flow, cosmetic outcome (two panels), no data impact — every primary flow is correct on both platforms.

**Fix (minimal, all existing contract strings survive — the onMouseDown count stays 2):** make the toggle's onClick pull focus into the field with the same guarded refocus —

```tsx
onClick={() => {
  setOpen((o) => !o);
  if (document.activeElement !== inputRef.current) {
    skipOpenRef.current = true;
    inputRef.current?.focus();
  }
}}
```

Opening arms the blur-close; closing refocuses the field with the flag consumed (onFocus returns before `setOpen(true)`, and the toggle's own setOpen stands). When the field already holds focus (the common case) the guard is false and behavior is unchanged. Pin with one source-contract assertion (the toggle's onClick contains the guarded refocus) if fixed; alternatively accept as a known edge case and note it in the doc comment.

### F2 restructure + contract strings (all checked line-by-line against a96ab1c)

The listbox div's only children are the mapped `role="option"` buttons (:126-162); the message div is a child of the un-roles panel (:121-124), never of the listbox. `aria-controls` is `listId` exactly when `open && visible.length > 0` — the one state where the `id={listId}` listbox renders (:119-126) — and `undefined` otherwise (closed, or open with an empty filter). No dangling reference in any state. Every round-1 contract string still matches, at its new line: `role="listbox"` ↔ :126, `role="option"` ↔ :131, `aria-autocomplete="list"` ↔ :91, `skipOpenRef.current = true;` ↔ :149, `inputRef.current?.focus();` ↔ :150, the consume regex (`if (skipOpenRef.current) {` / `skipOpenRef.current = false;`) ↔ :97-98, `rootRef.current?.contains(to)` ↔ :70, `onBlur={onBlur}` ↔ :76. The three round-2 contracts match too: the onMouseDown count-2 regex ↔ exactly two occurrences (:110 toggle, :133 entry — the doc comment does not contain the literal), `document.activeElement !== inputRef.current` ↔ :148, `visible.length > 0 ? listId : undefined` ↔ :90.

### Test evidence (stated by the implementer, statically verified, not re-run — read-only reviewer, no shell)

- **ModelCombobox.test.tsx: 13 tests counted** — 3 markup (:26/:35/:46) + 4 interactive (:55/:59/:64/:68) + 3 round-1 (:75/:81/:89) + 3 round-2 (:96/:103/:107), the round-2 describe titled "review round 2 — cross-platform + listbox structure" as described.
- **modelOptions.test.ts: 8 tests counted** (4 buildModelOptions + 4 filterModelOptions); **PricingSection.test.ts: 4 tests counted**. Neither file is touched by a96ab1c (the diff spans only ModelCombobox.tsx, ModelCombobox.test.tsx, and the round-2 report), consistent with round 2's verification at 4e9641e.
- **Count math:** 789 baseline + 22 (round 1) + 3 (round 2) = 814 ✓; ModelCombobox 10 → 13 ✓; no new test files in a96ab1c, so the 58-file count carries over from 4e9641e ✓. `npx tsc --noEmit` exit 0 is consistent by inspection — the diff adds no types or imports (the guard uses standard DOM properties).

### Commit contents & tree

a96ab1c is HEAD of `wt/agenticcoding` with parent 4e9641e (git log confirms both, matching the task's claim). The tree is clean (`git diff HEAD` and `git status --short` both empty). The round-2 report is included in the commit verbatim (50 lines, new file). The commit message accurately describes both fixes including the trap avoidance. Backlog 82dd66fc remains in_flight with plan_id ec52b07f — the done-flip belongs to the post-PASS closing sequence, consistent with the repo pattern.

### Notes (pre-existing, no action required — outside round-3 scope)

- **Escape on a keyboard-focused entry button** bypasses the input's Escape handler (it lives only on the input, :55-61) and bubbles to the dialog — closing the whole settings dialog rather than just the dropdown. Pre-existing since round 1; unchanged by a96ab1c.
- **Mousedown on the dropdown panel's own scrollbar** (lists longer than max-h-48): if the engine moves focus to body on scrollbar mousedown, the blur-close fires mid-scroll; wheel scrolling is unaffected. Pre-existing since round 1's L3; engine-dependent — worth a manual check if long model lists are common.
- `aria-expanded={open}` is true in the empty-filter state while `aria-controls` is undefined — permitted (aria-controls is optional in the ARIA 1.2 combobox pattern); noted for completeness.
