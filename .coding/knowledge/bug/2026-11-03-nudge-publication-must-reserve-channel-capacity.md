+++
title = "Nudge publication must reserve channel capacity before claiming"
created = "2026-11-03"
+++

Root cause A1: `claim_and_publish_nudge` set `nudge_sent = true`, appended the row, then used a non-blocking `try_send`; on a full or closed channel it only debug-logged and still returned true, so the one-per-session nudge was consumed with no published RoutingNotice and callers ignored the result. Fix: reserve channel capacity with `fanin_tx.reserve().await` BEFORE the route-state critical section (closed channel returns false, nothing claimed or logged). With the permit held, the claim, the synchronous nudge-row append and the infallible `Permit::send` all run under the single route-session mutex, so no await is held under the lock and a claim is always observable. The helper became `async` and both drift and strike callers await it. Regression `full_event_channel_never_consumes_the_nudge_silently` covers a pre-filled capacity-1 channel (nudge stays unclaimed until drained, then exactly one notice and one row) and a closed channel (no claim, no row). Verified red by temporarily restoring pre-fix try_send semantics. Fixed in commit `1aaab8a864d1b4895f48e90ee9ecf6f97ad11b2c`.
