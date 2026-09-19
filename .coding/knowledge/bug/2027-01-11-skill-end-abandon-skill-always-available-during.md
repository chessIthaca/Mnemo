+++
title = "skill_end/abandon_skill always available during a skill — MERGED into main (3ddfb62)"
supersedes = "dff6780f"
created = "2027-01-11"
+++

MERGED into main at 3ddfb62 (3ddfb621011d0092726cc1071ceb0c59c0f4a289) on 2026-09-19 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 2da86eb84b6a300dfc8452f2c37e5958afac6133) — supersedes this record's earlier unmerged status. Gist: symptom — during an active skill, skill_end/abandon_skill were only visible if the skill's allow-list named them, trapping the agent with no exit; fixed by adding them to the ToolFilter::Skill always-available set (plan dff6780f). Truth file: .coding/knowledge/bug/dff6780f.md.
