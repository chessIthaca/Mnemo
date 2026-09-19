+++
title = "reviewer-spawn gate denied the stringified \"null\" model artifact — FIXED (backlog 3e6f7887)"
created = "2027-01-11"
+++

BUG: reviewer-spawn gate denied the stringified "null" model artifact — FIXED (backlog 3e6f7887, plan 995436c2 follow-on). Symptom (live 2027-01-24, eight identical refusals): reviewer spawns omitting `model` were refused ("must OMIT the model parameter") — the strict-schema transport auto-fills omitted optional properties with null and stringifies them for string fields, so the gate saw model:"null" as an explicit pick; the correction was unsatisfiable and looped the main agent in Reviewing. Root cause: reviewer_spawn_gate (src/agent/dispatch.rs) treated any non-empty model string as a pick. Fix: gate + spawn_agent treat the literal "null" as absent/unset; the advertised model schema is ["string","null"]; a dropped "null" is named in the tool's success output (never silent, review L1). Regression tests: dispatch_treats_null_model_as_absent_for_reviewer_spawns (src/agent/tests.rs), null_model_string_counts_as_unset (src/tool/agent/spawn_agent.rs). Plan: .coding/plans/995436c2.md
