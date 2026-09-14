+++
title = "one frozen tool surface for all plan kinds + narrow landing sync tolerance — MERGED into main (4cbc3b0)"
supersedes = "2027-01-11-one-frozen-tool-surface-for-all-plan-kinds-narro"
created = "2027-01-11"
+++

MERGED into main at 4cbc3b0 (4cbc3b0b78bea5d4ab22967d3dd634b4851f9496) on 2027-01-11 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 4c08077). The decisions stand unchanged from the predecessor record (.coding/knowledge/decision/2027-01-11-one-frozen-tool-surface-for-all-plan-kinds-narro.md, which rides the merge): (1) Workflow::schema_filter advertises ONE byte-stable PlanFrozen surface for every plan kind — a per-kind research surface reset the provider prefix cache (measured 6,016-token collapse + 15.7-17.9 s TTFT) — while research restrictiveness stays at dispatch via ExecutingResearch allowed_tools(); (2) the app-managed landing syncs main with origin (fetch + pull --no-rebase) before its --no-ff merge, skipped when main tracks no upstream, surfaced when a tracked remote is unreachable. Guards: schema_filter_is_stable_across_plan_kinds, research_plan_still_cannot_write_despite_the_advertised_surface, land_syncs_main_with_its_upstream_before_merging, land_fails_when_a_configured_remote_cannot_be_reached, land_still_succeeds_without_any_remote. Review chain ended PASS: .coding/reviews/2026-09-14-round6-followups-review-round3.md.
