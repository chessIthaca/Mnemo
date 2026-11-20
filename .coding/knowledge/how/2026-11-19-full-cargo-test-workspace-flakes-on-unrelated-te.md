+++
title = "full cargo test --workspace flakes on unrelated tests here; cargo fmt is not clean on the base tree"
created = "2026-11-19"
evidence = "Six consecutive full-suite runs in one worker session each failed a different unrelated test; every one passed alone; final run green (commit 9f2bfe2d). `cargo fmt --check` output listed 235 files."
+++

On this Windows development machine, a full `cargo test --workspace` in Mnemo (about 940 team tests plus 3,600 lib tests in parallel) intermittently fails one test per run in an unrelated module — the failures observed were `mnemo2::team::proof::tests::dropped_proof_restores_the_checkout` (clean-tree assertion saw "M src.txt"), `headless_kernel::snapshot_reports_pending_approval`, `mnemo2::files_view::tests::lists_one_level_and_honours_gitignore` (a `git config` spawn failed), `runtime::agent::tests::idle_compaction_uses_the_measured_lifetime_and_records_the_row` ("database table is locked: savings_events"), and `runtime::agent::tests::cancel_during_approval_exits_agent`. Each failing test passes when run alone, and a later full run is green. When a full-suite run fails one unrelated test, rerun the suite instead of editing code.

Related: `cargo fmt --check` is not clean on the base tree (rustfmt 1.99 reports 235 files needing formatting), so `cargo fmt` is not the enforced style — never reformat the repo; at most format a newly added file.

Also: a test that sets a tight silence limit and also asserts that a second real command passes is load-sensitive — a 2 s silence limit killed a plain `git status` under full-suite load.
