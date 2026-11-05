+++
title = "CI-only failure modes — tauri default-run, Windows-only imports, Windows-only test helpers"
created = "2026-11-05"
+++

Three recurring CI-only failure modes in this repo, invisible on a Windows dev box but fatal under `#![deny(warnings)]` on the macOS job:

1. Tauri needs `package > default-run`. Once src-tauri/Cargo.toml declares any extra [[bin]] beside the app binary (currently mnemo2, mnemo2-mcp, mnemo2_route_eval, all required-features=["mnemo2"]), `tauri build` cannot pick the app and fails with "failed to find main binary, make sure you have a `package > default-run` in the Cargo.toml file". The key is `default-run = "mnemo-app"` in src-tauri [package]. Machine check: `cargo metadata --format-version 1 --no-deps --manifest-path src-tauri/Cargo.toml` must show default_run "mnemo-app" for the mnemo-app package.

2. Never put a Windows-only import in a shared `use`. src-tauri/src/main.rs imports from `mnemo::config`; `global_config_dir` was only used inside `#[cfg(windows)] fn browser_inspection_enabled()`, so the shared `use mnemo::config::{global_config_dir, ...}` was an unused import on macOS and broke the mac test build. Rule: name it by full path (`mnemo::config::global_config_dir()`) inside the cfg(windows) item, or cfg-gate the import itself.

3. Test-only helpers called only from cfg(windows) tests must be `#[cfg(all(test, windows))]`, not `#[cfg(test)]` — otherwise non-Windows test builds report "method X is never used" as dead code (case: LayaManager::with_serve_command in src/host/laya.rs). Before gating, check the field the helper sets: LayaManager::serve_override stays plain `#[cfg(test)]` because it is read in spawn_server's own `#[cfg(test)]` blocks, which are not Windows-gated.

Local checks that cover these on Windows: `cargo clippy --all-targets -- -D warnings`, `cargo clippy -p mnemo-app --all-targets -- [-features mnemo2] -- -D warnings`, `cargo test --lib`, `cargo build -p mnemo-app`. None of 2/3 reproduce locally — only the CI macOS job proves them.
