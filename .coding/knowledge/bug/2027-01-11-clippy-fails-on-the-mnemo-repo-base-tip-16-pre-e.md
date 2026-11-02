+++
title = "clippy fails on the mnemo repo base tip — 16 pre-existing findings under rust-clippy 1.95.0"
created = "2027-01-11"
+++

cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests exits 101 on the repository base tip (verified 2026-11-01 by stashing worker edits and re-running; identical failure with and without them; local toolchain rust-clippy 1.95.0, no pinned rust-toolchain.toml). 3 errors stop the build in the mnemo crate: src/headless/engine.rs:175 useless_format, src/headless/engine.rs:215 explicit_auto_deref, src/tool/agent/file_edit.rs:373 useless_format. With those two lints allowed via RUSTFLAGS, mnemo-app shows 13 more, all pre-existing: team.rs manual_inspect (758), too_many_arguments (1864 dispatch_on_plan, 2413 provision_worker, 4475 pump_worker), collapsible_match (4542, 4567), unnecessary_cast (5095), vec_init_then_push (5580); runtime.rs unnecessary_lazy_evaluations (580), let_and_return (796), too_many_arguments (1334 pump_events); ipc.rs:783 too_many_arguments; workbench.rs:271 manual_strip. Until these are fixed, clippy cannot pass on any branch off this line and never reaches a worker's changed files; workers can still prove their lines lint-clean by allowing just clippy::useless_format and clippy::explicit_auto_deref and confirming no findings in their diffs. Line numbers drift with local edits.

Resolved 2026-11-01: all findings (23 once the hidden ones surfaced) fixed on wt/mnemo; both clippy runs are clean.
