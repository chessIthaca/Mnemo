+++
title = "anthropic-workspace-id header + settings ErrorDialog (in main)"
created = "2026-09-01"
+++

SPEC: Anthropic workspace attribution + settings error dialog (plan f0338572) — in main at ae38f74 (2026-12-06, merge_to_main skill). (1) Workspace id: per-endpoint `workspace_id` in endpoints.toml (anthropic kind only — validate_endpoint clears it for other kinds) → AnthropicClientConfig.workspace_id → workspace_headers() → `anthropic-workspace-id` HTTP HEADER on every /v1/messages request (the single POST site). Mechanism: header-only, NEVER a body field (LiteLLM issue #29272); never sent empty; save-time validation rejects non-printable-ASCII (defense-in-depth eprintln at request time). (2) Settings save errors: ErrorDialog (frontend/src/components/settings/ErrorDialog.tsx, Radix ui/dialog) shows the FULL error text; inline strip is a clickable 2-line-clamped hint. Tests: 1714 passed / 0 failed, warning-free. Round-1 FINDINGS (0 high, 3 low) all fixed, round-2 PASS.
