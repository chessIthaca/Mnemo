+++
title = "Mnemo2 project cost uses durable raw per-model usage"
created = "2027-01-11"
+++

Mnemo2 persists lead and worker token usage per project and per model in the team ledger, independently of worker-card lifetime. TeamSnapshot exposes uncached input, cached input, output, integer micro-USD cost, and unpriced models; prompt token counts include cached tokens. Explicit endpoints.toml prices override vendor defaults. See src-tauri/src/mnemo2/usage.rs and mnemo2/src/lib/usage.ts.
