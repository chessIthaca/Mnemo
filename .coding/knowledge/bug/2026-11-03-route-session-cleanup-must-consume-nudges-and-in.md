+++
title = "Route-session cleanup must consume nudges and invalidate drift atomically"
created = "2026-11-03"
+++

Root cause H2: Clear consumed/logged an outstanding nudge and reset the route epoch in separate critical sections, allowing a drift claim between them to be erased without a nudge_ignored row; termination also left the old Settled/epoch usable, so a detached drift task could emit after the session ended. `clear_route_session` and `end_route_session` now atomically return any outstanding nudge while changing lifecycle and epoch under one std mutex. Clear and AgentTask termination pass that value to the shared row helper. End also sets Off{ended}. Regression tests: `drift_claim_during_clear_is_never_lost`, `drift_after_session_end_emits_nothing`, plus clear-then-end dedup. Fixed in commit `c6a951a54b3157da41e0459d22f821cce8aaa31d`; the same commit logs an intentional no-routine-target outcome as skip `kept`.
