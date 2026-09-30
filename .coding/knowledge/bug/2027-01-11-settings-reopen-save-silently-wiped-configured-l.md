+++
title = "Settings reopen+save silently wiped configured lane targets"
created = "2027-01-11"
+++

Symptom: with lane targets configured in [general.routing] (lane_medium/lane_high/escalate, landed by the lane-ladder item ad56c7bd), opening Settings and saving silently cleared them to unset.

Root cause: the GET wire `RoutingWire` (src-tauri/src/ipc/settings.rs, routing_wire) never emitted lane_medium/lane_high/escalate, while the frontend section read them and serialized them back — so a plain reopen+save sent explicit nulls. The save-side DTO already carried the lanes correctly (RoutingConfigDto, src/config/settings_dto.rs: absent = keep, null = clear); only the GET side was incomplete.

Fix: routing_wire emits all three lane fields (null when unset — EMITTED, never skipped); golden fixture dto-get-settings.json consciously updated alongside. Landed on wt/mnemo commit 6eff28a (budget item step 8, same wire surface).

Regression: `routing_wire_carries_the_lane_targets` (src-tauri settings tests) — proven red before the fix (lane_medium forced None → "a configured lane target must reach the wire", left: None, right: Some("mid")), green after.

Invariant: the GET wire MUST emit every field the save path distinguishes absent-vs-null (null = unset, never skipped).
