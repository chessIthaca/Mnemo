Mnemo2's Settings has one place to set a model's price, its row on the Endpoints page. The separate per-model pricing table on the Savings page and Mnemo2's whole-table pricing save are removed.

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
