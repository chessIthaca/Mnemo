+++
title = "Before a release tag, run the Rust suites the way CI does"
created = "2026-11-09"
+++

Run `cargo test --workspace --no-fail-fast`, then `cargo build -p mnemo2-app`, `cargo test -p mnemo-app --features mnemo2 --lib` and `cargo test -p mnemo2-app`. Package-scoped runs such as `cargo test -p mnemo` measure the tool-array budget without workspace feature unification and stay green while CI fails. `tools_array_stays_within_context_budget` is the usual casualty, so re-run it after any change to a tool description. CI's Windows job stops at the first failing test binary, so `--no-fail-fast` shows what it would hit next. On macOS the "Rust tests" step has continue-on-error, so its green tick means nothing: read the later "Fail if tests failed" step. new_release.toml only asks for builds, not tests. Tool descriptions: src/tool/agent/, src/tool/memory/, src/app/tool/workflow/; ceilings in src/agent/factory.rs.
