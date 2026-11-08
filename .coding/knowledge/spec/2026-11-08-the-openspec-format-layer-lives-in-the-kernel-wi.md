+++
title = "The OpenSpec format layer lives in the kernel with an app re-export"
created = "2026-11-08"
+++

All OpenSpec parsing and writing goes through `src/openspec/` (kernel, not feature-gated), so the kernel memory indexer `src/memory/indexer/openspec.rs` can use it. `src/app/openspec/mod.rs` re-exports it for plan-board and schema code and holds the app-only parts, such as `schema.rs`.
