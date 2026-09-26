+++
title = "Laya managed runtime — sidecar lifecycle + setup invariants — MERGED into main (f7e1fa5)"
supersedes = "2027-01-11-laya-managed-runtime-sidecar-lifecycle-setup-inv-3"
created = "2027-01-11"
+++

MERGED into main at f7e1fa5 (f7e1fa5d052c93068097a8078bf6e326ad935ce2) on 2026-09-26 via PR #7 (merge_to_main skill, PR path); branch wt/mnemo deleted both remotely (by the human) and locally (post-merge closeout step 3); pre-merge tip 2203439. Subject of this record: Laya managed runtime — sidecar lifecycle + setup invariants — its landed work was already in main via PR #6 (merge 614c42f); PR #7 carried the branch's remaining work plus the bookkeeping records. Nothing is pending on this record.

Amended 2027-01-11: The `mode`/`checkpoint` config fields, the checkpoint SELECTION and the `laya_setup(checkpoint)` argument were REMOVED by plan 2f74e10a (2027-02, wt/mnemo): `checkpoint_catalog()` now returns exactly one entry (English, live `installed` flag + size for the Settings card) behind `english_checkpoint()`, `laya_setup()` takes no argument, and setup autostart reduces to `setup_autostart_decision(laya) = (laya.enabled, !laya.enabled)` re-derived from the LIVE config at completion (test: `ipc::laya::tests::setup_autostart_decision_follows_the_live_config`). The sidecar lifecycle invariants above (loopback bind, free-port pre-allocation, stop-before-restart, per-PID log, per-checkpoint markers, zip-slip safety) are unchanged.
