+++
title = "all failing tests must be fixed — no tolerated failures (review gate)"
created = "2026-08-30"
+++

DECISION (user, 2026-09-19): All failing tests must be fixed before a plan can proceed to review — no tolerated failures, ever. A test that cannot run deterministically in the default `cargo test` suite (e.g. needs a graphical WebView2/Chromium environment unavailable in the headless runner) must be marked `#[ignore]` with a clear reason comment, so it's explicitly opt-in (`cargo test -- --ignored`) and the default suite is always green and trustworthy. Flaky/failing tests left in the default suite erode trust in the entire suite — once you learn to ignore some failures, real regressions slip through. This is a REVIEW GATE: the reviewer must verify `cargo test` is fully green (zero failures) before PASS; `#[ignore]` tests are acceptable (explicit opt-out with reason), flaky/failing tests in the default suite are not. The agent must run `cargo test` and confirm zero failures before spawning the reviewer — never proceed with known failures.
