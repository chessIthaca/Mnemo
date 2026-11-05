+++
title = "trace_view.rs DTOs flatten the kernel record — new fields serve automatically, only the test fixture needs them"
created = "2026-11-05"
+++

Mnemo2's Tauri trace DTOs (src-tauri/src/mnemo2/trace_view.rs) do NOT re-declare the kernel record fields: `TraceRow` and `TraceDetail` carry `#[serde(flatten)] summary: LlmRequestSummary` / `record: LlmRequestRecord`, so any field added to the kernel structs (src/provider/trace.rs) serves to the frontend wire JSON automatically — no DTO code change needed. The ONLY explicit construction of those kernel structs in trace_view.rs is the test fixture (`summary()`/`record()` in the `#[cfg(test)]` module), which lists every field: adding kernel fields breaks that fixture's compile until the new fields are added there. Consequence for parallel work: a frontend/DTO-side worker cannot add fixture fields before the kernel-side change lands (its own cargo would go red), so the fixture update belongs to whoever integrates after the kernel change merges.
