+++
title = "widening an age/elapsed assert in Mnemo tests must keep clippy's Range::contains form"
created = "2026-11-26"
evidence = "Commit 80246f61 (plan bb7b3cbe step 1): clippy error \"manual `Range::contains` implementation\" at src-tauri/src/mnemo2/team/tests.rs:20697 under #![deny(warnings)]; fixed to (30..30 + 120).contains, clippy then clean and mnemo2::team::tests 429 passed."
+++

When a test window is widened, write the predicate as `(lo..hi).contains(&x)`, NOT `x >= lo && x < hi`: clippy's `manual_range_contains` is denied through `#![deny(warnings)]` (set in src-tauri/src/mnemo2/team.rs and the crate roots), so the `&&` form fails `cargo clippy --workspace --all-targets` with "manual `Range::contains` implementation". The two predicates are identical for a half-open range, so a plan quoted as `offline >= 30 && offline < 30 + 120` becomes `assert!((30..30 + 120).contains(&offline), "{entry}")`.

Also: `eventually` in src-tauri/src/mnemo2/team/tests.rs now waits on a std::time::Instant deadline of 120 s (poll 25 ms), not 800 iterations; its panic text is "the polled condition never became true within 120 s".
