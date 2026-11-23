+++
title = "the tier revision the lead's results and team_tiers read"
created = "2026-11-22"
evidence = "tests team_tiers_reports_models_health_and_run_as, a_tier_change_is_flagged_until_the_lead_reads_team_tiers (the identical-ladder case failed before the ladder comparison)"
+++

routing.rs TierChanges (shared through UsageLimits.changes, because worker pumps mark usage limits) holds a revision, the last change time and the revision the external lead last saw. set_tiers bumps it only when the ladders differ — it compares tier_ladder_from_dto(..).ordered(), never the DTOs, because TiersDto.resolves_to is output-only and goes stale without any real change — and never on the runtime's first ladder (the starting setup). mark_model_unavailable bumps it too. Attaching an external lead and calling team_tiers from it mark the revision seen; until then external-lead team_dispatch results carry the 'Tiers changed at HH:MM ...' line in warnings, and team_resume/team_route_fix carry it as a line after the JSON (they have no warnings array). team_tiers.resolves_to follows the dispatch rule: an agent tier resolves to claude-code/<model> while Claude Code leads, with mnemo_resolves_to giving what audits/escalation run on.
