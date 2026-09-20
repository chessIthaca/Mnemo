+++
title = "wedged offscreen browser hangs tools forever — no CDP timeouts (plan ec425270) — MERGED into main (752164e)"
supersedes = "2027-01-11-wedged-offscreen-browser-hangs-tools-forever-no"
created = "2027-01-11"
+++

MERGED into main at 752164e (752164e420903fa893f4449c4fb02f85a1911dd7) on 2026-09-20 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip c91650c; work commit 880736c) - supersedes this record's earlier "branch wt/mnemo @ bc3fd5e (unmerged - exists only on this branch)" marker. Symptom: offscreen_browser_* calls hang forever when the shared headless Chromium is wedged (alive process, open CDP, unresponsive renderer, e.g. infinite JS loop); the watchdog never reaps it. Root cause: no launch-mode CDP round-trip was bounded by a timeout. Fix: the cdp() wrapper (30s ops / 60s navigate) + force_reap_wedged + transparent respawn on the next op, with the generation guard and the watchdog probe (see the SPEC record). Regression test: wedge_renderer_times_out_and_self_heals (cargo test --features browser wedge_renderer -- --ignored). Full detail: .coding/knowledge/bug/ec425270.md.
