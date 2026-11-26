+++
title = "src/remote is only compiled with the `remote` feature"
created = "2026-11-25"
evidence = "src/remote/mod.rs is behind #[cfg(feature = \"remote\")] in src/lib.rs; `cargo test -p mnemo --features remote remote::` ran 31 tests green; Cargo.toml feature `remote = [\"headless\", \"dep:async-tungstenite\", \"dep:hmac\"]`."
+++

mnemo's `src/remote` module (the hub↔node link and pairing) is compiled only under the `remote` feature, which implies `headless`. `cargo test -p mnemo` with default features skips it entirely; test it with `cargo test -p mnemo --features remote remote::`, and check the no-feature build with `cargo build -p mnemo --no-default-features --features kernel,headless`. The workspace build gets `remote` through `src-tauri/mnemo2-app`'s mnemo dependency. The link tests use `tokio::io::duplex` (no ports); hand-built-frame tests drive a raw WebSocket side and stamp frames with `remote::auth::frame_mac`.
