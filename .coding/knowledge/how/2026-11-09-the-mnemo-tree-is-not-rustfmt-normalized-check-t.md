+++
title = "the Mnemo tree is not rustfmt-normalized — check the baseline before trusting a fmt failure"
created = "2026-11-09"
evidence = "`cargo fmt --all -- --check` → 238 distinct files listed; rustfmt --check --edition 2024 on the untouched output_compactor.rs at HEAD → 33 hunks; shell_filter.rs → exit 0."
+++

The Mnemo workspace does not satisfy the installed rustfmt. Evidence (2026-11): `cargo fmt --all -- --check` reports diffs in 238 files, and `rustfmt --check --edition 2024 src/tool/agent/output_compactor.rs` reports 33 hunks on the pristine HEAD file (pre-existing: e.g. the classify_cargo_build prefix array and every `assert!(cond, "{}", arg)` line — this rustfmt wraps fn-call args over fn_call_width=60). Toolchain: rustc/rustfmt 1.9.0-stable (2026-04-14); crate edition 2021; no rustfmt.toml; no `cargo fmt` job in .github/workflows.

Consequence for future work: a `rustfmt --check` verify line failing on a touched file is NOT by itself evidence the edit is unformatted — run the same check on the pre-edit baseline first. Do not reformat a whole file just to satisfy it: that buries the reviewed diff in hundreds of unrelated lines. Follow the file's existing style instead. Files that DO pass (e.g. src/tool/agent/shell_filter.rs) show the check is meaningful per-file, so the baseline test is the discriminator.
