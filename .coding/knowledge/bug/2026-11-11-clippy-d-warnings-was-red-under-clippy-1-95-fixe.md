+++
title = "clippy -D warnings was red under clippy 1.95 (fixed 2026-11-11); no CI runs clippy"
supersedes = "2026-11-08-clippy-d-warnings-is-red-on-the-base-tip-under-c"
created = "2026-11-11"
evidence = "Lead step-6 run on wt/mnemo (both clippy runs exit 101 with these five lints); worker tip 5688661c re-run by the lead: both exit 0, 0 warnings."
+++

Five lints under #![deny(warnings)] made `cargo clippy --workspace --all-targets` and `cargo clippy -p mnemo-app --all-targets --features mnemo2` fail. The two in src/app/plan_board.rs (:418 redundant_guards, :1508 nonminimal_bool) were already red when v2.2.0 shipped. Three more: src/runtime/agent.rs:1820 drop_non_drop (a no-op drop of a tokio::pin! handle), src/agent/optimizer.rs:752 needless_update, src/memory/tests.rs:3809 useless_vec. All five were fixed on wt/mnemo in merge b9cbaee4 (plan 3ce7d708), with no behaviour change. Root cause of the drift: PR CI runs only CodeQL and build.yml runs only on tags or manually, so clippy and tests are never gated on PRs; lints pile up between releases. Run both clippy commands before any release or wt/mnemo PR.
