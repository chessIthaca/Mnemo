+++
title = "retired savings lever kinds stay in the ledger but the reports skip them"
created = "2026-11-10"
evidence = "src/memory/tests.rs savings_reports_skip_retired_kinds; user: \"I also still see delta reads in the cost panel\""
+++

When a savings lever is removed, its kind goes into RETIRED_SAVINGS_KINDS (src/memory/mod.rs; today delta_read and skeleton). savings_stats (total, per-kind, per-day, recent) and savings_valuation filter it out through reported_kinds_sql(); savings_events_rows, the raw audit reader, still returns those rows, and nothing is deleted. Why: the Cost tab's savings are all-time, so a retired kind would keep its own row forever, and the user wanted removed levers gone from the panel. Filtering in the engine, not in the Svelte view, keeps the headline total, the lever rows, the day chart and the dollars in agreement, and Mnemo1's dashboard follows automatically. To retire another lever: add its kind to the constant and delete its LEVERS entry in mnemo2/src/lib/cost.ts.
