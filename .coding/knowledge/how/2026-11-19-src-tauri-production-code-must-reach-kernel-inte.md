+++
title = "src-tauri production code must reach kernel internals through the mnemo::headless facade"
created = "2026-11-19"
evidence = "2026-11-19 plan d3336a8a step 6: `cargo test --workspace` failed at tests/boundary.rs:155 with `mnemo::host` at src-tauri/src/mnemo2/runtime.rs:272,516,518; fixed by `pub use crate::host::config_watcher::{ConfigWatcher, CONFIG_DEBOUNCE};` in src/headless/mod.rs. Boundary suite then passed."
+++

tests/boundary.rs `tauri_uses_only_public_api` bans the bare tokens `mnemo::agent`, `mnemo::runtime`, `mnemo::host` (plus `use mnemo as`, grouped `mnemo::{agent|runtime|host`) in src-tauri production sources. A new lib type the app crate needs (e.g. `host::config_watcher::{ConfigWatcher, CONFIG_DEBOUNCE}`) must be re-exported from `mnemo::headless` (or `mnemo::app`) first and imported from there; `cargo test -p mnemo --test boundary` is the fast red check (it only fails after the lib compiles).
