## Verdict: FINDINGS (0 high, 3 low)

Both features are correctly implemented end-to-end. All four review asks check out: the effort resolution precedence, cache key, and serde round-trips are correct; the windowing memo/expander logic is correct and the streaming/auto-scroll path is untouched; the ~65 script-inserted test literals are clean; and both new frontend test files are genuinely collected by vitest (the +19 test-count math matches exactly). The three LOW findings are polish/bookkeeping: a false-dirty edge in the Models section draft, the still-pending backlog item for the now-shipped windowing, and a missing PLAN.md bullet for feature 2.

### Scope

- Full `git diff HEAD` on wt/agenticcoder (19 files, +566/−49) plus 5 untracked files: `Conversation.window.test.tsx`, `ModelsSection.test.tsx`, two `.coding/plans/` files, one `.coding/knowledge/how/` file.
- Feature 1 (plan 11f8c13c): per-context reasoning-effort overrides in `[models]`.
- Feature 2 (plan 51b12662): conversation transcript windowing.

### Feature 1 — per-context reasoning-effort overrides (verification detail)

**Resolution precedence (verified end-to-end):**
- config.toml `[models]` → `ModelRef.reasoning_effort` (`#[serde(default, skip_serializing_if = "Option::is_none")]` — old configs parse, unset slots serialize byte-identically; parse + round-trip tests pin both).
- `Config::resolve_model_ref` (src/config/mod.rs) carries the field; every `ConfigModelResolver::resolve` arm (skill > subagent > state) routes through it, so all six slot kinds carry the effort.
- `resolve_turn_provider` (loop_impl) → `build_turn_provider(&model_ref)` — the state-model path carries the effort into both the cache key and the build.
- `build_provider_for` → `resolve_effort` (client_factory): a set context effort wins, normalized through `normalize_reasoning_effort_for`; unset → `effective_reasoning_effort_for` (the model's own chain). Effective precedence = context > per-model > endpoint > `"max"`, exactly as documented in README/PLAN.
- The `normalize_reasoning_effort_for` extraction is behavior-preserving (checked line-by-line against the old body: supports gate first, allow-list clamp to the list's first entry, off→off_wire encoding incl. the DeepSeek name policy + config override; the `configured` computation moved ahead of the call but is side-effect-free; `unwrap_or("max")` ≡ the old `None => Some("max")` arm; `model_id.unwrap_or_default()` handling identical).
- Anthropic: `build_client`'s Anthropic branch takes no effort parameter — structurally ignored, matching the docs; the UI hides the dropdown for anthropic hosts. An effort set on a slot later pointed at an anthropic endpoint stays dormant in config, never sent.
- Toolbar runtime override stays on top: `set_model` (src-tauri/src/ipc/agent.rs) builds a provider with the requested effort and swaps it into the loop as the picker pin, which beats the config chain — the README claim is accurate.

**Cache key:** `(endpoint, model, effort)` keyed on the RAW effort string. Distinct efforts → distinct providers; same triple → same `Arc` (the new test pins both directions). `set_config` clears the cache, so Settings saves rebuild providers. Two contexts whose efforts normalize to the same value (e.g. both clamp to the allow-list's first entry) get distinct cache entries — one redundant provider at most, never incorrect.

**429-sticky reroute:** `sticky_endpoint` matches on model id + failed endpoint only — the new `reasoning_effort: None` lookup literals cannot miss a sticky record because of the effort field. The recorded alternate (from `find_alternate_endpoint`, effort `None`) rebuilds without the context effort — the documented, intentional limitation (PLAN.md + the two loop_impl comments). The forced-model and default-provider paths have the same shape and the same comments.

**Serde round-trips (both directions):**
- Save: frontend `ModelRefConfig.reasoning_effort` → `ModelRefDto` (serde default; `to_ref` trims, empty→None — the whitespace-only case is tested) → config.
- Load: `model_ref_wire` carries the field for all five fixed slots + the skill map (`models_config_wire`, settings.rs:678-691); `skip_serializing_if` keeps unset slots' wire shape identical (tested — the key is asserted absent).
- The UI sentinel (`__default__` ↔ null) round-trips cleanly; `effortToSelectValue(undefined/null/"")` → sentinel, `effortFromSelectValue(sentinel)` → null. The dropdown offers exactly `REASONING_EFFORTS` = max/high/medium/low/minimal/off, matching the Rust doc set.

**Script-inserted test literals (~65):** reviewed every hunk in the diff — each insertion is `reasoning_effort: None,` on its own line with sibling-matching indentation and a trailing comma; the two settings_dto.rs hunks additionally fixed pre-existing missing trailing commas (rustfmt-consistent). No mangled formatting or wrong insertion points found. The green `cargo test` under `#![deny(warnings)]` at both crate roots also proves compile-completeness (a missed `ModelRef` literal would not compile).

### Feature 2 — transcript windowing (verification detail)

- `windowEntries`: correct tail-window semantics; under the cap the SAME array is returned (identity pinned via `toBe`) — zero allocation and no behavior change for short transcripts.
- Memo: filter → window → group → chunk, with `windowSize` in the deps (pinned by both test files). `hidden` is the post-filter hidden count, so the expander label counts exactly what clicking would reveal. A window boundary can cut mid-turn; the pre-existing `grouped.length === 0` guard makes the partial first turn its own group (cosmetic only).
- Expander: renders only when `hidden > 0`, grows by `TRANSCRIPT_WINDOW` per click, disappears at full visibility. `windowSize` never shrinks — bounded by the store's MAX_TRANSCRIPT_ENTRIES (1000), so at most a handful of clicks.
- Live stream: the streaming block renders inside the LAST turn's group (plus the empty-transcript fallback) — the tail always contains the last turn, so the stream is never windowed away (pinned by test).
- Auto-scroll: effect deps + endRef-after-turns placement unchanged (pinned); `windowSize` is deliberately NOT in the scroll deps, so clicking the expander does not yank the user to the bottom.
- No new dependency (pinned: no react-window/virtuoso).

### Review asks 3 + 4

**Ask 3 (script-inserted literals):** clean — see above.

**Ask 4 (vitest collection):** both new files are explicitly listed in `frontend/vitest.config.ts` `test.include` — `src/components/chat/Conversation.window.test.tsx` (line 20) and `src/components/settings/sections/ModelsSection.test.tsx` (line 30) — and both paths match the untracked files exactly. The pre-existing glob only covers `settings/**/*.test.ts`, so the `.tsx` files genuinely required the explicit entries; both were added. The `?raw` + `renderToStaticMarkup` pattern is established (ModelCombobox.test.tsx). The reported count math corroborates collection: Conversation.window.test.tsx has 14 tests, ModelsSection.test.tsx has 5 → +19, and 978 + 19 = 997 reported. Both files' tests genuinely exercise the new behavior (they fail without the change: msg-049 would render unwindowed, the expander label would be absent, the Effort dropdown markup would be absent).

### Project review expectations

- **Documentation sync:** README updated for both features — each claim verified against the code (per-context precedence, anthropic ignore, toolbar-on-top, window size, expander label). PLAN.md updated for feature 1 only → LOW 3. No shipped `.toml` example carries `[models]`, so nothing else to update. Public functions have doc comments (`normalize_reasoning_effort_for`, `resolve_effort`, exported `ModelPickerBody` / `windowEntries` / `TRANSCRIPT_WINDOW`).
- **Multi-platform neutrality:** no Windows-only APIs, paths, or shell syntax anywhere in the diff. Pass.
- **Warning-free build:** reported green at both crate roots under `#![deny(warnings)]`; no `#[allow]` added; the new pub method is consumed. Pass.
- **Regression tests:** both features ship tests that fail without the change (windowing render pins; effort precedence / cache-key / wire pins). No defect fixes in this diff, so no BUG memory is required.

### Findings

**LOW 1 — Models section reports dirty after a net-zero effort edit (false-dirty).**
`frontend/src/components/settings/sections/ModelsSection.tsx`. After load, a slot with no effort has NO `reasoning_effort` key (the wire omits it via `skip_serializing_if`). Picking an effort and then reverting to "model default" leaves `reasoning_effort: null` in the draft; `JSON.stringify` emits `"reasoning_effort":null` while the snapshot lacks the key, so `dirty` stays true for a semantically net-zero edit (the OK button stays enabled; saving is idempotent and harmless — `to_ref` normalizes null→None). Fix: canonicalize on load — in `draftFromSettings`, map each fixed slot and skill ref through `{ ...ref, reasoning_effort: ref.reasoning_effort ?? null }` so loaded and edited states stringify identically (a revert then clears dirty like every other field).

**LOW 2 — backlog item b2cb83b6 (transcript windowing) still `pending`/in-flight although the feature ships in this diff.**
`.coding/backlog.jsonl`. The item's new note ("plan loop did not close … work kept in tree") is accurate pre-commit, but if the item stays `pending` a future run-all dispatch would re-implement the already-shipped windowing. When committing this work, flip the item to `done` with the commit pointer (the established pattern in the neighboring done items).

**LOW 3 — PLAN.md has no bullet for the transcript-windowing decision.**
Feature 1 got a PLAN.md section; feature 2's deliberate technical decision (option (a) windowing over virtualization, K=200, virtualization deferred pending profiling) is documented only in README + code comments + the backlog item. PLAN.md's feature/decision list is where comparable decisions live (e.g. the per-model endpoint config bullet directly above the new effort bullet). Add a short bullet mirroring the README one and noting the deferred-virtualization choice.

### Notes (no action required)

- Anthropic-host rows render two children in the `sm:grid-cols-3` grid (the effort column is hidden) — the two dropdowns occupy 2/3 of the width. Cosmetic; each row is its own grid, so cross-row column alignment was never a thing.
- The expander's post-click scroll position works because the button sits at the top of the windowed list (the user has scrolled up to click it), so the newly revealed batch starts right below it; native scroll anchoring covers the rest. No action needed.
- Legacy id-less transcript entries (pre-entryId saves) use the `t${ti}` position-fallback key, which shifts when the window grows — a remount of those rows on an explicit expander click only; the primary entryId key is unaffected.
- `resolve_effort` is computed (then dropped) for anthropic-kind endpoints in `build_provider_for` — one wasted normalization, no behavior impact.
- The two `.coding/plans/` files and the `.coding/knowledge/how/` vitest-include file are consistent with the changes and should ride the commit; the knowledge file documents exactly the trap LOW 2's sibling feature avoided (test files silently not collected).
