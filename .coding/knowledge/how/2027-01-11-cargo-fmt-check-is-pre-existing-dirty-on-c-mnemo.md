+++
title = "cargo fmt --check is pre-existing-dirty on C:/Mnemo — not a gate; only fix hunks in lines you add"
created = "2027-01-11"
+++

cargo fmt --check fails repo-wide on C:/Mnemo even for committed, untouched files (verified 2026-09-27: src/agent/turn.rs, src/agent/context.rs, src/agent/startup.rs, src/tool/agent/output_compactor.rs, src-tauri/src/ipc/laya.rs all flagged; none in the working diff) under installed rustfmt 1.9.0-stable (59807616e1 2026-04-14) with no rustfmt.toml/rust-toolchain pin. The committed tree was formatted under a different style edition (rustfmt wants e.g. `Self { dir: dir.into() }` collapsed but `log.record("x", &[..], Action::Y)` expanded). Consequences for workflow: (1) do NOT treat fmt-clean as a completion gate — the gate is un-piped `cargo test` green under #![deny(warnings)]; (2) never reformat a whole touched file (it would balloon the plan diff with pre-existing churn); (3) do fix fmt hunks that fall inside lines the plan itself adds, when cheap. Plan 1d36f28d followed this: index_staleness.rs + factory.rs:1493 hunks fixed, rest left alone.
