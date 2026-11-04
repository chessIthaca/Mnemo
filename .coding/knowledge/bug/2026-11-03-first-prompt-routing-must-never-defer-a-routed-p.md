+++
title = "First-prompt routing must never defer a routed provider swap"
created = "2026-11-03"
+++

Root cause N1: `pin_routed_provider` used the deferring explicit-pin path, so a routed swap blocked on compaction could complete after a later manual pick and reinstall Laya's model. First-prompt routing now checks `swap_needs_compaction` for the target context first; if true it writes `skip: "needs_compaction"`, emits no routed ModelChanged, and settles with `model: None`. Otherwise `pin_routed_provider` installs immediately and never creates `pending_swap`, with a debug assertion. Regression `routed_target_needing_compaction_is_not_applied`. Also N2: `session_routing_enabled` now excludes subagents and `skip_turn_routing` sessions, so Mnemo2/worker model changes write no Mnemo1 session `reroute` rows; provider changes still apply. Tests `skip_turn_routing_session_model_change_writes_no_session_row` and `subagent_model_change_writes_no_session_row`. Fixed in commit `f5f244850e46b3e8275cec63532cb37a43af3e0d`.
