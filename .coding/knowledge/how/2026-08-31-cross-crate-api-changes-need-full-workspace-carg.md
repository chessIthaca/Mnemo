+++
title = "cross-crate API changes need full-workspace cargo test"
created = "2026-08-31"
+++

Process lesson from the 2026-12-16 remediation-sweep review (finding 6): when a commit changes AgentLoop's public constructor surface (e.g. the AgentLoopConfig refactor, L2), `cargo test --lib` alone is INSUFFICIENT — the Tauri app crate (src-tauri/) has its own test call sites (e.g. src-tauri/src/ipc/spawn.rs) that also construct AgentLoop and will break. Always run the FULL workspace test (`cargo test --lib` AND `cargo test --manifest-path src-tauri/Cargo.toml`) after any change to a cross-crate public API, not just the lib crate. The L2 commit shipped with a broken spawn.rs test that was only caught later during the M1a extraction's Tauri build.
