+++
title = "read-registry cap test evicted concurrent tests' live receipts (process-wide registry)"
created = "2026-11-18"
evidence = "`cargo test --workspace` failed on `tool::agent::read_registry::tests::shift_cap_drops_the_receipt` at src/tool/agent/read_registry.rs:589 until `the_cap_evicts_the_oldest_receipt` was localized; see the commit on branch wt/agent-missExce (395f22e6), after which the workspace suite is green."
+++

`src/tool/agent/read_registry.rs` has a process-wide `REGISTRY` with `MAX_RECEIPTS = 512` and `prune_oldest` (oldest by `seq`). The cap test used to mint 513 receipts into that global registry and thereby evicted every other live receipt in the test binary, so a test that mints and then keeps working (`shift_cap_drops_the_oldest`-style: mint, 64 `note_write` calls, then `resolve`) could lose its own receipt mid-test — a latent cross-test flake any added mint surfaces. The cap policy is now asserted on a LOCAL `Registry { receipts, next_seq }` value plus a bystander-receipt check. Rule of thumb: a test that mints into the global registry and does meaningful work before resolving is exposed to eviction by any other mint-heavy test running in parallel.
