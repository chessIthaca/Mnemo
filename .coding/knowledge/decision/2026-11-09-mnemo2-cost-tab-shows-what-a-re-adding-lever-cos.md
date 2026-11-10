+++
title = "Mnemo2 Cost tab shows what a re-adding lever cost, as a cost"
supersedes = "2026-11-02-mnemo2-cost-tab-negative-levers-render-as-cost-r"
created = "2026-11-09"
evidence = "cost.test.ts 'a re-adding lever shows what it cost'; insights.rs a_re_add_costs_its_tokens_and_a_request; decided in the savings-lever plan on 2026-11-09."
+++

A lever that re-adds tokens (`archive_expand`, `outline_followup`) renders as a cost row: meta "+120,000 tokens re-added · 6 events", dollars "costs ≈ $0.04" from `estimated_cost_micros`, and no dollars only when nothing is priced. This reverses the earlier no-dollars rule, which existed because the backend priced negatives at $0 and any figure next to a minus read as a saving. A real figure now exists: the tokens back at the session's rates plus one re-sent prompt per event, and the wording says "costs". The Savings headline nets savings against these costs, clamped at 0. See mnemo2/src/lib/cost.ts leverRows and src-tauri/src/mnemo2/insights.rs savings_report.
