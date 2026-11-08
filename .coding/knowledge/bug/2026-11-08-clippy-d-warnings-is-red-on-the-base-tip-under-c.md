+++
title = "clippy -D warnings is red on the base tip under clippy 1.95 (plan_board.rs:418, :1508)"
created = "2026-11-08"
evidence = "`cargo clippy --all-targets -- -D warnings` run twice in a worker worktree: with a change (2 errors at plan_board.rs:418,1508) and with the change stashed at base tip dc5108b3 (identical 2 errors, exit 101); `git blame -L 418,418` and `-L 1508,1511` both show commit e238cf799; no rust-toolchain.toml exists (rustc 1.95.0, 2026-04-14)."
+++

Any worker running the standard verification `cargo clippy --all-targets -- -D warnings` in this repo currently fails on two PRE-EXISTING lints that have nothing to do with its own change:

- src/app/plan_board.rs:418 `Some(reason) if reason.is_empty() =>` — clippy::redundant_guards (suggests `Some("")`)
- src/app/plan_board.rs:1508 `if !config.governed_files.get(&delta.capability).is_some_and(|files| !files.is_empty())` — clippy::nonminimal_bool (suggests `is_none_or`)

Both lines blame to commit e238cf799 (2026-11-05). Verifying on the clean base tip dc5108b3 with any working change stashed reproduces the identical two errors, exit 101. There is no rust-toolchain.toml, so the machine's rustc/clippy 1.95.0 is used, and `#![deny(warnings)]` at src/lib.rs:5 turns both into hard errors.

Practical consequence: before blaming yourself for a clippy failure, check whether the flagged lines are in your diff (`git diff`, `git blame`); if not, report them as pre-existing instead of fixing out-of-scope production code, and never add `#[allow(...)]` (project rule: fix root cause, which here means the two small expression rewrites clippy suggests).
