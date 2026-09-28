+++
title = "docs/costsaving.md — local-only cost-saving orchestration prompt (gitignored; backlog af2a2be6)"
created = "2027-01-11"
+++

docs/costsaving.md (full path C:\Mnemo\docs\costsaving.md) is a LOCAL-ONLY, gitignored file — never stage or commit it (.gitignore rule added 2026-09-28 alongside the marketing local-only section). Content: a prompt image transcribed to text with "Jev" renamed to "Laya" (including the tag rename jev_schema -> laya_schema; TYPESAFE_API_KEY and the Luna/Sol/Opus 5.5 model names are kept verbatim). It specifies a three-layer cost architecture — BRAIN (Opus 5.5: planning, decomposition, escalations), WORKERS (Luna lanes at low/medium/high reasoning), REFLEX (Laya: every bounded routing decision in ~81ms via a typed single-call schema on complexity / action / scope_drift / confidence / risk_flag) — plus deterministic-verification-first evidence gathering, a hard budget layer (max tokens per task, 3 retries per lane, 1 Sol escalation, kill switch at the spend cap), nightly routing-threshold self-tuning, and a per-task cost report.

Backlog item af2a2be6-0ca5-4a9f-bfe9-bbd4840dfc47 (pending) asks for the architecture discussion: adopt / reject / map-to-current-code for each prompt section, anchored on this app's plan-first guardrails, four-tier memory (working -> episodic -> semantic -> procedural), subagent routing, and the seven optimizer levers (src/config/general.rs::OptimizerConfig).
