+++
title = "Run clippy on both Mnemo crates; a failing crate hides later findings"
created = "2027-01-11"
+++

The repo has two clippy runs, and both must pass (warnings are errors): `cargo clippy --lib --tests` (crate mnemo) and `cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests` (crate mnemo-app, which depends on mnemo). When the mnemo crate fails, the mnemo-app run stops before linting its own code. Temporarily allowing the failing lints (`-- -A clippy::<lint>`) shows more, but findings in further targets (src-tauri/src/console.rs, the mnemo2-mcp binary) can still stay hidden until the earlier errors are fixed. So when sizing a lint cleanup, expect more findings to surface after the first fixes, and re-run both commands until both finish clean. Lint fixes are mechanical work for the simple tier. too_many_arguments is handled with a targeted allow and a one-line reason, not by refactoring signatures.
