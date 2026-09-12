+++
title = "vision support is per-model (ModelSpec.multimodal overrides endpoint flag)"
created = "2026-09-01"
status = "superseded"
+++

DECISION: vision (multimodal) support is a per-MODEL option layered over the endpoint default — ModelSpec.multimodal: Option<bool> overrides Endpoint.multimodal via Endpoint::multimodal_for(model_id) (override wins, None inherits; explicit Some(false) forces stripping even at a multimodal endpoint). Rationale: one Ollama endpoint hosts mixed models (text-only GLM + vision-capable GLM flash). All client-construction sites resolve per model (build_provider_for, main provider, save re-sync, resolve_model_provider, memory maintenance); the vision client keeps its forced-true override; a vision-capable active model never falls back to general.vision_model (runtime gate is_multimodal reads the built provider's caps). UI: Settings → Endpoints per-model "vision" checkbox (checked=on, unchecked=inherit; explicit false endpoints.toml-only — tooltip documents all three states). Shipped in f3b70a7 + 57b95c6 on wt/agenticcoder (not yet merged to main). Plan: .coding/plans/d5d43154.md; reviews: .coding/reviews/2026-09-18-per-model-multimodal-review{,-round2}.md (r2 PASS).
