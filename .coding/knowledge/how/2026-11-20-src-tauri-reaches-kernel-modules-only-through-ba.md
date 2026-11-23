+++
title = "src-tauri reaches kernel modules only through bare crate-root re-exports (boundary test)"
created = "2026-11-20"
evidence = "tests/boundary.rs; src/lib.rs re-export block."
+++

tests/boundary.rs tauri_uses_only_public_api scans all src-tauri production code, binaries included (only #[cfg(test)] modules are exempt), for the path tokens mnemo::agent, mnemo::runtime and mnemo::host, and their grouped forms. A violation fails cargo test --workspace. A src-tauri file that needs a kernel module reaches it through a BARE crate-root re-export in the sanctioned block in src/lib.rs. The re-export needs a consumer-named comment, the same #[cfg(feature = ...)] gate as the module it re-exports, and must be one name deep (for example `#[cfg(feature = "embeddings")] pub use crate::host::models_host;`, which the app then uses as mnemo::models_host). Fast probe: `cargo test -p mnemo --test boundary` (about 30 s) instead of the full workspace run.
