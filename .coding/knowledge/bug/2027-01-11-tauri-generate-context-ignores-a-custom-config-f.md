+++
title = "Tauri generate_context ignores a custom config filename"
created = "2027-01-11"
+++

Symptom (2026-10-24): `cargo run -p mnemo-app --bin mnemo2` created a visible untitled window, stayed alive with 2 idle threads, loaded no WebView2 modules/child, and never created %LOCALAPPDATA%\com.mnemo.mnemo2; the frontend never appeared. Root cause: `tauri::generate_context!("mnemo2.conf.json")` does NOT load that custom file. The macro resolves the path against CARGO_MANIFEST_DIR and validates the filename, but `tauri-codegen::get_config` then uses only the path's parent directory, and `tauri-utils::config::parse::read_from` always loads `<dir>/tauri.conf.json`. So Mnemo2 compiled Mnemo1's `com.mnemo.desktop` context and collided with Mnemo1's live WebView2 profile. Proof: `cargo test -p mnemo-app --lib mnemo2::tests::context_uses_mnemo2_identifier` failed with left `com.mnemo.desktop`, right `com.mnemo.mnemo2`. Fix direction: put the alternate config at its own `<dir>/tauri.conf.json` and pass that path; keep a context-identifier regression test. Plan 52d7bb81.
