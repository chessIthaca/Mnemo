+++
title = "cargo fmt is red repo-wide and is NOT a gate — never run cargo fmt, match surrounding style by hand"
created = "2026-11-11"
evidence = "cargo fmt --check on a clean tip (2027-01-13): 251 files / 2895 diffs across untouched files; repo has no rustfmt.toml; no CI workflow runs fmt"
+++

`cargo fmt --check` is RED on every clean branch of this repo under the current default toolchain's rustfmt: ~251 files / ~2895 diffs on a clean tree, in files nobody touched (e.g. src/agent/tests.rs, src-tauri/mnemo2/mcp_server.rs). The repo has NO rustfmt.toml and rustfmt is NOT a gate anywhere — no CI job runs fmt, and the verify lists for plans use the two clippy runs (`cargo +1.99.1 clippy --workspace --all-targets` and `cargo +1.99.1 clippy -p mnemo-app --all-targets --features mnemo2`), which do pass. Practical guidance: never run `cargo fmt` (it would reformat the whole repo into a giant diff), never treat fmt output as evidence your change is wrong, and don't waste cycles "fixing" fmt drift — match the surrounding committed style by hand. To judge your own delta's formatting, compare against the immediate surrounding code, not rustfmt. Evidence: a clean-tip `cargo fmt --check` in a worker worktree listed 251 files including dozens untouched by the commit under review.
