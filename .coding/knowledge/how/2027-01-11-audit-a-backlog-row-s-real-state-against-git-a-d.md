+++
title = "audit a backlog row's real state against git — a dead plan loop leaves delivered work in_flight/pending"
created = "2027-01-11"
+++

HOW: a backlog row's status tracks the PLAN lifecycle, not delivery — a plan loop that dies (provider stream error, user interrupt, "plan loop did not close") leaves DELIVERED work sitting in_flight, and an item never dispatched through a loop sits pending even though its work shipped. Both read as "still executing" in the Backlog tab and have fooled the user (2026-09-26 post-PR #7 closeout: e4a50d22 "Token-optimizer parity" and 85313a7e "Reviewer rounds 2+" were in_flight with their work in main as 1a37d2f / a85de74; ebe21e3c "Optimizer levers in Settings" was pending with its work in main as 2890ad0).

Before trusting the tab, map each suspicious row to the tree, cheapest first: (1) `git log --oneline -500 | Select-String -Pattern "<item-id-8>"` — commit messages carry "(backlog <full id>)"; (2) search .coding/plans/*.md for the item id to see whether a plan was ever dispatched and how far it got; (3) for a no-commit match, treat the row as genuinely open (the Laya-chain items 091e694d / e2c47d5f, fe499e37, 8ea77b61 and the two deferred rows had no landing commit). Then `backlog_status` → done with a note citing the landing commit and why the row was stale. Read-only audit: do NOT flip rows without the user's go-ahead.
