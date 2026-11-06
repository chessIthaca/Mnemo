+++
title = "Mnemo2 model prices are edited only on the Endpoints page"
created = "2026-11-05"
+++

Mnemo2 has exactly one place to set a model's price: that model's row on Settings > Endpoints (EndpointsPage.svelte → Setup::upsert_endpoint_detail, saving `model.price` into the shared `[[pricing]]` rows of endpoints.toml). There is no whole-table pricing editor: `model_settings_apply` accepts only "models", "vision", "embedding", "optimizer" and "budget" and rejects "pricing" with `unknown model-settings section '<x>' (expected models, vision, embedding, optimizer or budget)`; ModelSettingsWire has no `pricing` field; SavingsPage.svelte takes only a `transport` prop and shows "Model prices are set with each model on the Endpoints page."; modelSettings.ts has no PricingRow/pricingDraft/pricingWire/parsePrice/modelIdOptions. Unchanged: Config.pricing/PricingEntry, src-tauri/src/mnemo2/pricing.rs (resolve_rates + vendor defaults), all cost math, and Mnemo1's SavingsSection.tsx plus the shared PricingDto/SettingsSaveDto.pricing that Mnemo1 still uses.
