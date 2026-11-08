+++
title = "clippy for tests/boundary.rs needs -A clippy::redundant_guards -A clippy::nonminimal_bool"
created = "2026-11-07"
+++

The repository's Rust tree does not currently pass a bare `cargo clippy --test boundary -- -D warnings`: clippy stops while checking the `mnemo` lib (before the boundary test target compiles) on two pre-existing lints in `src/app/plan_board.rs` — `redundant guard` at 409:29 (surfaced by `#![deny(warnings)]`, declared at src/lib.rs:5) and `nonminimal_bool` at 1501:16. The file's last commit is e238cf79.

Lint the boundary test target cleanly with the two allowances:
`cargo clippy --test boundary -- -D warnings -A clippy::redundant_guards -A clippy::nonminimal_bool` → exit 0.

Do not add `#[allow]` in plan_board.rs or edit it from a tests/boundary.rs task; `cargo test --test boundary` alone is unaffected (it compiles without `-D warnings`).
