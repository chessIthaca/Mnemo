+++
title = "a reviewer that fails without a report — harvest its scratch, verify the tree, ask"
created = "2027-01-11"
+++

When a spawned role:"reviewer" agent finishes WITHOUT a report (the honest signal since 5b46674d): (1) do NOT respawn blindly — the harness refuses it while the failure is pending; ask_user for retry-on-another-model vs abandon-review. (2) HARVEST its session output before anything else: its scratch lives under .coding/tmp/ and often holds real evidence — the failed reviewer for plan 7d2d63f6 (2027-01-13) left .coding/tmp/review-wry057/ with its independent red-check runs (g-A-registry-wry.out and g-B-patch-unused.out FAILED exactly as predicted, g-C-real.out passed on the real lock) plus files it saved for fidelity comparison; those outputs are quotable evidence even though no verdict exists. (3) VERIFY TREE INTEGRITY: a reviewer doing red-checks may swap real files — that one doctored Cargo.lock and restored it, so check `git status --short` + `git diff --quiet -- <file>` after any failure and restore with `git checkout --` if dirty. (4) Fold the harvested evidence into the plan context so a retry reviewer or a resumed session starts informed.
