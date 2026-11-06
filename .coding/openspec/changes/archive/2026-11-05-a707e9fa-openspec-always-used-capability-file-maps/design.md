## Interfaces

- 2026-11-05: config.yaml contract: governed_files maps a capability name to a list of repo-relative file paths (globs not required in v1 — exact paths); parsed by OpenSpecConfig as a new field; empty or absent lists mean no gating for that capability.
- 2026-11-05: plan_create contract: gains an optional explicit no-spec-impact declaration with a reason (the worker designs the exact field shape and mirrors it in the tool schema); the refusal message names each ungated-for capability and both remedies.
- 2026-11-05: File ownership: this plan owns src/app/openspec/schema.rs, src/app/openspec/capability_spec.rs, src-tauri/src/mnemo2/mcp_server.rs, the plan-validation site, .coding/openspec/config.yaml, and their tests — disjoint from plan fdc334b4's files (mnemo2/src/lib/trace.ts, TraceView.svelte and tests).
