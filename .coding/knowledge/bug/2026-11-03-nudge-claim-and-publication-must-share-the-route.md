+++
title = "Nudge claim and publication must share the route-state critical section"
created = "2026-11-03"
+++

Root cause K2: drift and strike paths claimed a nudge under `route_session`, released the lock, then appended the row and awaited `fanin_tx.send`. Clear could consume the claim and write `nudge_ignored` before the nudge was publicly visible, or a blocked send could publish after Clear. Production now uses `claim_and_publish_nudge`: while holding the same std mutex after a successful claim, it appends the nudge row synchronously and calls `fanin_tx.try_send`; dropped notices are logged in debug builds, and no lock crosses an await. Bare `claim_nudge` remains test-only for linearization races. Regression `claimed_nudge_is_published_before_clear_can_run` verifies event order, nudge-before-ignored row order, and no post-Clear arrival. Fixed in commit `f30442e0eb6bc4f72118091380ae308fc2c723ff`.
