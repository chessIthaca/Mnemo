+++
title = "Mnemo and Mnemo2 ship as separate installers, and only CI proves macOS"
created = "2026-11-05"
status = "superseded"
+++

Mnemo2 is its own Cargo package `mnemo2-app` in src-tauri/mnemo2-app (bins mnemo2 = src/main.rs and mnemo2-mcp = src/mnemo2_mcp.rs; mnemo2_route_eval is an example). Its installer comes from `npx tauri build` run in that directory; Mnemo's from `npx tauri build` at the repo root. The Tauri CLI bundles every file and folder under a package's src/bin regardless of Cargo bin settings, so neither package may keep extra programs or module folders in src/bin; mnemo-app keeps default-run = "mnemo-app" and autobins = false. On macOS the second tauri build clears bundle/dmg, so build.yml stages each app's dmg and .app.zip into dist-macos right after its own build. Installed mnemo2.exe uses windows_subsystem (no console); mnemo2-mcp stays a console binary. Developing on Windows does not compile macOS/Linux cfg blocks or run macOS timing, and CI uses the newest stable Rust (deprecations become errors under deny(warnings)), so before a tag run `gh workflow run build.yml --ref <branch>` and list the artifacts; run `cargo clippy --workspace --all-targets -- -D warnings` locally because CI does not run clippy. A known-flaky macOS failure can be re-run with `gh run rerun <id> --failed`.
