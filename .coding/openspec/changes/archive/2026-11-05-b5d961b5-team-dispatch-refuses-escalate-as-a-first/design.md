## Decisions

- 2026-11-05: The mnemo2 team module is feature-gated, so verification runs with `--features mnemo2` (`cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team`). This step also fixes the two pre-existing unused imports in src-tauri/src/ipc/finetune.rs (BTreeMap, training_log_dir) by gating them like their usages, so `cargo clippy -p mnemo-app --all-targets -- -D warnings` passes both with and without `--features mnemo2`.
