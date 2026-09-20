//! Single merged integration-test binary.
//!
//! The four suites below used to be four separate `tests/*.rs` targets —
//! four separate binary links (~1.5-2 min of extra link time per full
//! `cargo test --workspace` run, each binary linking the whole `mnemo`
//! stack). Merging them into one binary with a shared `main.rs` turns
//! four link targets into one (link-time research 2026-12-07, lever #5).
//! The suites are self-contained (no cross-module fixtures); test names
//! keep their original `module::test` paths.

mod ci_workflow;
mod contract_fixtures;
mod ipc_bridge;
mod workflow_integration;
