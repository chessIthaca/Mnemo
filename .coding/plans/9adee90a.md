# Plan: Mnemo2 prices are set in one place: with the model on the Endpoints page

## Goal
Mnemo2's Settings has one place to set a model's price, its row on the Endpoints page. The separate per-model pricing table on the Savings page and Mnemo2's whole-table pricing save are removed.

## Kind
implementation

## Context
WHY: the user found two places to set costs in Mnemo2 and wants only the one beside the model. Both edit the same `[[pricing]]` rows in the shared `endpoints.toml`: EndpointsPage.svelte edits `model.price` per model (saved by `Setup::upsert_endpoint_detail` in src/headless/setup/endpoint_detail.rs), and SavingsPage.svelte edits the whole table (saved by `model_settings_save` section "pricing" in src-tauri/src/mnemo2/model_settings.rs). No data moves; this removes the second editor only.

DECISIONS ALREADY TAKEN:
- The Endpoints page stays exactly as it is: $ in, $ out on the row, Cached price under the row's advanced details.
- The Savings page keeps its levers and knobs, and gains one short line pointing to Endpoints for prices.
- `[[pricing]]` in endpoints.toml, `Config.pricing`, `PricingEntry`, src-tauri/src/mnemo2/pricing.rs (`resolve_rates`, vendor defaults) and all cost calculation are unchanged. Existing rows must survive every remaining save.
- Mnemo1 is untouched: frontend/src/components/settings/sections/SavingsSection.tsx keeps its table, and the shared `PricingDto` and `SettingsSaveDto.pricing` in src/config/settings_dto.rs stay because Mnemo1 uses them.
- Do not move SavingsPage within SettingsDialog's load gates and do not touch the unsaved-edits guard beyond the one trackUnsaved line named in the step.

VERIFY (run in the worktree):
- `cd mnemo2 && npx vitest run`
- `cd mnemo2 && npx svelte-check --tsconfig ./tsconfig.json`
- `cargo test -p mnemo-app --features mnemo2 --lib model_settings`
- `cargo clippy --workspace --all-targets -- -D warnings`

## Acceptance scenarios
- Settings > Savings shows the levers and knobs, no per-model pricing table, and a line pointing to the Endpoints page for prices.
- Settings > Endpoints still shows and saves input, output and cached prices per model.
- model_settings_save with section "pricing" is refused as an unknown section and leaves [[pricing]] in endpoints.toml unchanged.
- Saving another settings section keeps every [[pricing]] row and the tier ladder.

## Spec changes

### settings-dialog

#### ADDED Requirements

##### Requirement: Model prices have one editor
Mnemo2's settings dialog SHALL edit model prices only with each model on the Endpoints page, and the Savings page SHALL NOT offer a pricing table.

###### Scenario: Savings page points to Endpoints
- **WHEN** the user opens the Savings page
- **THEN** no pricing table is shown
- **AND** a line says prices are set with each model on the Endpoints page

## Steps
- [x] 1. **Remove the Savings pricing table and Mnemo2's pricing section** Files: mnemo2/src/components/SavingsPage.svelte, mnemo2/src/components/SavingsPage.test.ts, mnemo2/src/components/SettingsDialog.svelte, mnemo2/src/components/SettingsDialog.test.ts, mnemo2/src/components/ModelsPage.test.ts, mnemo2/src/lib/modelSettings.ts, mnemo2/src/lib/modelSettings.test.ts, mnemo2/src/test/previewSettings.ts, mnemo2/src/test/previewScenes.ts, src-tauri/src/mnemo2/model_settings.rs, src-tauri/src/mnemo2/mod.rs. A. SavingsPage.svelte: - Remove the `<section aria-label="Per-model pricing">` block, the `<datalist id="savings-page-models">`, the `pricing` state, `pricingDirty`, `savePrices`, `discardPrices`, `addRow`, `removeRow`, `options`, the `.pricing-table` CSS rules, and the imports `Plus`, `X`, `modelIdOptions`, `pricingDraft`, `pricingWire`, `PricingDraftRow`, `EndpointDetailDto`. - Remove the `endpoints` prop, so the only prop is `transport`. - `trackUnsaved(() => optimizerDirty);` - In the load effect, set only the optimizer draft and original. - Inside the Savings levers section, directly after the existing `<header>` paragraph, keep the header as is and add one new paragraph at the end of the section's content, before `.section-actions`: `<p class="micro">Model prices are set with each model on the Endpoints page.</p>`. - Update the doc comment at the top to describe levers and knobs only, saved as the "optimizer" section. B. SettingsDialog.svelte line 252: `<SavingsPage transport={modelTransport} />`. Change nothing else there. If `endpoints` is still used elsewhere in the file, leave it. C. mnemo2/src/lib/modelSettings.ts: remove `PricingRow`, the `pricing` field of `ModelSettings`, `"pricing"` from `ModelSettingsSection`, `PricingDraftRow`, `pricingDraft`, `parsePrice`, `pricingWire`, `modelIdOptions`. Keep `modelRefOptions`, `parseWholeNumber` and all budget and optimizer helpers. Update comments that mention pricing. D. mnemo2/src/test/previewSettings.ts: remove the `pricing` seed, the `case "pricing"` branch and the `PricingRow` import. previewScenes.ts line 2257: change `Savings page (levers, pricing)` to `Savings page (levers)`. E. Frontend tests: - SavingsPage.test.ts: remove the `pricing` fixture, `sampleEndpoints`, the `emptyModel` and `EndpointDetailDto` imports, the `endpoints` render prop, and the tests "edits prices and saves the pricing section", "adds and removes pricing rows", "skips blank-model rows on save", "rejects a non-numeric price inline". Rename the first test to drop the pricing table and remove its two row assertions (`Model for row 1`, `Input price for row 1`). Update the file's doc comment. - Add test `it("has no pricing table and points to the Endpoints page")`: render, await the levers, assert `screen.queryByRole("region", { name: "Per-model pricing" })` is null, `screen.queryByLabelText("Model for row 1")` is null, `screen.queryByRole("button", { name: "Save prices" })` is null, and `screen.getByText("Model prices are set with each model on the Endpoints page.")` exists. - modelSettings.test.ts: remove the imports and assertions for `parsePrice`, `pricingWire`, `modelIdOptions`. Keep the `modelRefOptions` assertion; if a test name then misdescribes it, adjust the name. - SettingsDialog.test.ts lines 449 and 602, ModelsPage.test.ts line 43: remove the `pricing` fixture fields. F. src-tauri/src/mnemo2/model_settings.rs: - Remove `ModelSettingsWire.pricing`, the `PricingWire` struct, its mapping in `model_settings_read`, `apply_pricing`, the `"pricing"` match arm, and `PricingDto` from the import. - The unknown-section message becomes `(expected models, vision, embedding, optimizer or budget)`. Update the doc comments that list sections or pricing: the module header lines 15-16 and 29-30, `model_settings_apply`, `model_settings_save` (including its "new models and prices" wording). - Tests: remove the `wire.pricing` assertions at lines 716-720. Replace `save_pricing_replaces_the_table_and_keeps_tiers` with two new tests: 1. `pricing_section_is_refused_and_leaves_the_table_alone`: `model_settings_apply(&setup, "pricing", json!([]))` returns an error containing `unknown model-settings section`, and `Config::load(dir.path())` still has `pricing_for("plan-model")` with `input_per_1m == 1.5`. 2. `other_saves_keep_the_pricing_table`: save the "budget" section with `json!({ "max_tokens_per_plan": 999 })`, then `Config::load` still has `plan-model` at input 1.5, output 2.5, cached 0.1, and the tier ladder's routine tier is still `["ep1/exec-model"]` (load it the way the removed test did). - Keep the `[[pricing]]` block in the test fixture. G. src-tauri/src/mnemo2/mod.rs line 127 doc comment: remove any mention of the pricing table if it has one; otherwise leave it. Do not touch: EndpointsPage.svelte, TiersPage.svelte, src/headless/setup/endpoint_detail.rs, src/config/*, src-tauri/src/mnemo2/pricing.rs, usage.rs, trace_view.rs, insights.rs, team.rs, anything under frontend/, README.md, docs/. Done when: all four verification commands pass, and `Grep` for `pricingWire|pricingDraft|modelIdOptions|parsePrice|PricingRow|PricingWire|apply_pricing` under mnemo2/src and src-tauri/src/mnemo2 finds nothing.

## Progress
- 2026-11-05 step 1 done: step 1 merged: priceOneHome01 (priceOneHome01) on deepseek-v4.1-flash:1017, merge commit cf6d9460; review: Savings keeps only levers and knobs with a line pointing to Endpoints; Mnemo2's pricing wire field and save section are gone; the [[pricing]] rows, cost calculation, Endpoints page and Mnemo1 are untouched.
