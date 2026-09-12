## Verdict: FINDINGS (0 high, 3 low)

Implementation plan ec52b07f — "Pricing model field: editable combobox of configured models" (backlog 82dd66fc): the change is **correct, well-tested, platform-neutral, and doc-synced**. The core feature (dropdown of configured endpoints' models + free typing, save path untouched) is verified sound. The three findings are all combobox interaction polish — ARIA listbox semantics, keyboard focus after selection, and cross-row open-state coordination — none affect the feature's correctness or the save path.

### Scope reviewed (all uncommitted changes on `wt/agenticcoding`)

- **Tracked (git diff HEAD):** `README.md` (:120 clause), `frontend/src/components/settings/sections/PricingSection.tsx`, `frontend/vitest.config.ts`, `.coding/backlog.jsonl` (82dd66fc → in_flight, bookkeeping).
- **Untracked (new):** `frontend/src/components/settings/sections/modelOptions.ts`, `.../ModelCombobox.tsx`, `.../modelOptions.test.ts`, `.../ModelCombobox.test.tsx`, `.../PricingSection.test.ts`.
- A usage search for `ModelCombobox|buildModelOptions|filterModelOptions` confirms the new modules are referenced only in the files above (+ the plan file and the vitest include) — no stray or missed wiring.

### Findings

#### L1 (a11y, low) — popup lacks listbox semantics; `aria-controls` points at a plain div

`ModelCombobox.tsx:79-108` — the dropdown container referenced by `aria-controls={listId}` has no `role="listbox"`, and the entry buttons have no `role="option"`; the input also lacks `aria-autocomplete="list"`. The requested basics are all present (`role="combobox"` :56, `aria-expanded` :57, `aria-controls` :58, `aria-label` on the toggle :69, `autoComplete="off"` :59), but per the ARIA 1.2 combobox pattern the controlled popup should be announced as a listbox — today a screen reader gets no "list with N options" context when the dropdown opens.

**Fix (attributes only, no behavior change):** `role="listbox"` on the popup div (:80-82), `role="option"` on each entry button (:90), `aria-autocomplete="list"` on the input (:54-66). Optionally pin with a source-contract assertion in `ModelCombobox.test.tsx`.

#### L2 (keyboard UX, low) — keyboard selection drops focus to `<body>`

`ModelCombobox.tsx:89-106` — entries are real focusable buttons, and since there is no arrow-key navigation, the only keyboard path to an entry is Tab → Enter. Selecting unmounts the focused button, so focus falls back to `<body>`; the next Tab restarts from the top of the document. Mouse users are unaffected.

**Fix:** return focus to the field on selection — add an input ref and call `inputRef.current?.focus()` in the entry `onClick` (:93-96) after `setOpen(false)`. **Caveat:** the input opens on `onFocus` (:63), so a naive refocus would immediately reopen the dropdown — guard it with a one-shot flag (e.g. `skipOpenRef.current = true` before focusing; in `onFocus`, consume the flag and skip `setOpen(true)` once). Programmatic `.focus()` fires `onFocus` synchronously, so the flag is consumed in the same tick.

#### L3 (UX, low) — no cross-row coordination / blur close: two dropdowns can be open at once

`ModelCombobox.tsx:32` — each row's combobox owns an independent `open` state; nothing closes row A's dropdown when the user moves to row B. Because row B's input wrapper (`relative z-20`, later in DOM) paints above row A's click-outside overlay (`fixed inset-0 z-10`, :46-52), clicking row B's model input while row A's dropdown is open focuses it and opens a **second** dropdown — both render simultaneously, and it takes two further clicks (one per overlay) to clear them. Tabbing out of the combobox likewise leaves the dropdown open (only overlay click / Escape / select / toggle close it).

The in-repo precedent solves this: EndpointCard's model picker keeps a single shared `pickerOpen` row-index state (`EndpointCard.tsx:685-703`), so only one picker can be open per card. **Minimal fix without lifting state:** close on input blur when focus leaves the combobox root — `onBlur={(e) => { if (!rootRef.current?.contains(e.relatedTarget)) setOpen(false); }}`. The option buttons live inside the root, so clicking an entry does not close before its click fires; `relatedTarget` is `null` for clicks on non-focusable areas (the overlay), which closes correctly. Keep the overlay for the toggle-focused case. This fix is compatible with the L2 fix (focus moves from an option button to the input, both inside the root — no root-exit blur).

### Verified correct (no action)

- **Data layer** — `buildModelOptions` groups/dedups across endpoints, annotates with endpoint names, skips blank ids, sorts (modelOptions.ts:31-48); `filterModelOptions` implements the documented rule (empty/whitespace → all, exact match → all, else case-insensitive substring, no match → []). All pinned by 8 unit tests whose expectations match the implementation.
- **Wiring** — PricingSection builds options from `getSettings().endpoints` in `load()` (:42), refreshing on section activation; the combobox is controlled by `r.model` and updates via the same `updateRow` path the old input used (:130-134).
- **Save path unchanged** — trim + numeric coercion + filter non-empty + `saveSettings({ pricing: cleaned })` (:69-77); no membership validation, so free-typed values still save. The negative source contracts in PricingSection.test.ts:47-54 hold against the actual source.
- **Escape handling is correct** — SettingsDialog closes via Radix `onEscapeKeyDown` → `requestClose()` (SettingsDialog.tsx:211-214), a document-level listener. ModelCombobox's `e.stopPropagation()` on the React synthetic keydown stops the native event at the React root, so Radix never fires: the dropdown closes, the dialog stays open. Escape with the dropdown closed propagates and closes the dialog through the dirty-check flow — correct.
- **Context claims verified** — `EndpointInfo` carries `name` + `models` (tauri.ts:279; consumed at modelOptions.ts:33-34; typecheck-verified); pricing is exact-name keyed on both sides — `Config::pricing_for` (src/config/mod.rs:92-94) and StatsView `modelCost` (StatsView.tsx:57-59, `fmtCost(0)` → "—"). The combobox addresses a real silent-failure mode.
- **No reuse missed** — no pre-existing combobox in the repo (`role="combobox"` has zero matches outside the new file); only plain `<select>`s.
- **No dead code / unused imports** in any changed file; all imports used.
- **Test conventions followed, zero contract drift** — node-env vitest: pure-helper unit tests + `?raw` source contracts (ChatSection/InflightBar pattern) + `renderToStaticMarkup` for closed-state markup (MnemoLogo/SplashCard pattern). All 18 new tests' contract strings were checked line-by-line against the actual sources — every one matches (e.g. `onChange={(e) => onChange(e.target.value)}` = ModelCombobox.tsx:62, `onChange(o.model);` = :94, `buildModelOptions(s.endpoints)` = PricingSection.tsx:42, `saveSettings({ pricing: cleaned })` = :77).
- **Styling** — the input keeps the section's exact input classes (border-border / bg-bg-secondary / text-xs / focus accent) with `rounded-l` + attached chevron (`rounded-r border-l-0`); the dropdown uses border-border / bg-bg-primary / shadow-lg with muted annotation text — consistent with the design system.

### Requested checks

1. **Documentation sync** — README.md:120's new clause ("editable combobox listing the models the configured endpoints serve, annotated with their endpoint names, plus free typing for anything else") matches the implementation exactly. PLAN.md has no pricing-model-field text to go stale (only general Stats/pricing feature mentions at :929/:931). Module doc comments on both new files are accurate.
2. **Multi-platform neutrality** — frontend-only diff; no platform-specific code, paths, or APIs; Rust untouched.
3. **A11y basics** — all four requested basics present (see L1 for what's missing beyond them).
4. **vitest config** — `ModelCombobox.test.tsx` added to the explicit include list (vitest.config.ts:27) in path-sorted position — the established pattern for `.test.tsx` files (MnemoLogo, SplashCard); the settings glob only matches `*.test.ts`, and `modelOptions.test.ts` / `PricingSection.test.ts` are covered by that glob. Correct.
5. **Dead code / contract drift** — none found (see above).

### Notes (no action required)

- The exact-match-shows-all filter rule means the list momentarily jumps from filtered to all while typing toward an exact match (e.g. typing "glm-5.2" character-by-character). Judged acceptable: documented, deliberate (reopening after a selection browses everything), and tested.
- While a dropdown is open, the fixed overlay consumes the first outside click (e.g. clicking "Add pricing row" first just closes the dropdown) — the standard click-outside-overlay tradeoff.
- `key={i}` row indexing in PricingSection is pre-existing and untouched by this diff.
- Test evidence (`npx tsc --noEmit` exit 0; 58 files / 807 tests passed) is stated, not re-run (read-only review); static verification of every changed line is consistent with it.
