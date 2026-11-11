+++
title = "One Settings > Tiers page drives tier routing for Mnemo1 and Mnemo2"
created = "2026-11-04"
status = "superseded"
+++

Mnemo2's Settings > Tiers page is the only place for models per tier and for who picks the tier; the Decisions tab is gone. Its top control, "Does Laya help pick the tier?", saves in one call (save_tiers_top_control in laya_settings.rs): No (mode off, laya.routing off), Watch only (shadow, laya.routing on), Suggest (suggest, laya.routing on), Auto (auto, laya.routing on). It does not write or read Mnemo1's enforce, which only Mnemo1's own Classifier settings change. Laya's readiness (install, turn on, retry, ready with the checkpoint and the kNN labelled-worker count) is shown in place. Below it come the five-tier ladder editor (fold hints, a required trivial tier, Laya label chips), a preview of the team_dispatch tier text, collapsed routing details with the eval (gates min_confidence_4/3/2), and a Laya section with the non-routing flags. Laya's routing question has four labels (trivial, routine, involved→medium, hard→high); four-label answers are gated by min_confidence_4 (default 0.40). Old tier ids (simple, average, difficult) are refused everywhere. Stored data was converted once: endpoints.toml, team.json (backup team.json.pre-tiers.bak) and mnemo2.toml (rewritten on load). Mnemo1's settings UI keeps only enforce and the thresholds.
