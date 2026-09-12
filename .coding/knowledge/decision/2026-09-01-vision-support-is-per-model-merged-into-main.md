+++
title = "vision support is per-model — MERGED into main"
supersedes = "2026-09-01-vision-support-is-per-model-modelspec-multimodal"
created = "2026-09-01"
+++

DECISION: vision support is per-model (ModelSpec.multimodal overrides endpoint flag) — MERGED into main at 4173dda (2026-09-19), branch wt/agenticcoder deleted. Gist unchanged: ModelSpec.multimodal: Option<bool> overrides Endpoint.multimodal via Endpoint::multimodal_for(model_id); vision-capable active models receive image blocks directly (no vision-model fallback); Settings → Endpoints per-model "vision" checkbox (unchecked = inherit; explicit false endpoints.toml-only).
