+++
title = "mnemo2 host Rust tests are feature-gated — plain cargo test --workspace skips them; run cargo test -p mnemo-app --features mnemo2 --lib"
created = "2027-01-11"
+++

Mnemo2 host code (src-tauri/src/mnemo2/**) is behind the non-default Cargo feature `mnemo2` (src-tauri/Cargo.toml [features] `mnemo2 = []`; src-tauri/src/lib.rs `#[cfg(feature = "mnemo2")] pub mod mnemo2`). Plain `cargo test --workspace` therefore does NOT compile or run any mnemo2 host tests — a filtered run of an mnemo2 test name reports "0 passed, N filtered out" and exits 0 (misleadingly green). The gate that actually compiles and runs the mnemo2 lib tests is `cargo test -p mnemo-app --features mnemo2 --lib` (documented in .github/workflows/build.yml lines 68/154). Any change under src-tauri/src/mnemo2/** must be verified with that feature-enabled run in addition to the default `cargo test --workspace`. Confirmed live: both the pre-existing worker test `compact_resumes_one_stopped_worker_and_persists_the_real_outcome` and a newly added lead compact test match 0 tests under plain workspace filters, and the new test passes under the feature-enabled run.
