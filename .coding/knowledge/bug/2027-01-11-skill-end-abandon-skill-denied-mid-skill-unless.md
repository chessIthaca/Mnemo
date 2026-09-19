+++
title = "skill_end/abandon_skill denied mid-skill unless the allow-list named them — FIXED (plan dff6780f)"
created = "2027-01-11"
+++

Symptom (user report 2027-01-24): "skill_end is always allowed during a skill. Not having that is silly." During an active skill, skill_end (exit to target_state) and abandon_skill (rollback) were only visible/callable if the skill's allow-list named them — a skill file omitting them trapped the agent mid-skill with no exit and no rollback, while the injected skill prompt (src/agent/prompt.rs:770) advertised both.

Root cause: the ToolFilter::Skill arm (src/tool/mod.rs:829-857) auto-granted only memory tools, ask_user, current_plan, skill_reload and the backlog tools; the exits fell through to allowed.iter().any(|n| n == name), so an allow-list without them hid the only way out.

Fix: both exits joined the Skill arm's always-available set (the never-stuck rule, mirroring abandon_plan in the Reviewing arm). SKILL_FILE_HEADER and the skill_create schema description now document that the exits need not be listed in `tools`; listing them stays legal (redundant, not denied).

Regression tests: skill_exits_are_always_available_inside_a_skill (src/tool/mod.rs — both exits allowed under ToolFilter::Skill(vec![]), the exact pre-fix failure shape; hidden under Planning/Executing/Reviewing/Complete; not implicitly granted to Reviewer) and dispatch_allows_skill_end_even_when_the_allow_list_omits_it (src/agent/tests.rs — start_skill with allow-list ["file_read"], then skill_end succeeds through the dispatch gate; pre-fix it failed with "not allowed ... Skill").
