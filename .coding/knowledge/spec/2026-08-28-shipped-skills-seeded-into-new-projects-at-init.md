+++
title = "shipped skills seeded into new projects at init"
created = "2026-08-28"
+++

SPEC/DECISION 2026-02-13: Shipped skills are seeded into every project at Project::init (plan b33d21c9, commits 281a0f3 + 0bb534d on wt/agenticcoder, review round-2 PASS, 1569 tests green). Mechanism: src/skill/mod.rs SHIPPED_SKILLS (&[(&str,&str)] via include_str! of the repo's .coding/skills/*.toml — compile-embedded, installed copies need no source tree; adding a shipped skill = drop the TOML + add one entry) + seed_skills(skills_dir) (create_dir_all + write-if-missing per skill); wired into BOTH Project::init paths (fresh scaffold + already-initialized self-heal, mirroring the agent.md re-scaffold precedent) — called from create_project IPC and the CLI --project path only, not on every app open. User-modified skill files are never overwritten; deleted files/dir self-heal on next init. Registry consumes them at startup via SkillRegistry::load_dir(&project.skills_dir). Reviews: .coding/reviews/2026-02-13-seed-skills-into-projects-review.md (0 high/2 low, fixed) + -verify-review.md (PASS). Not yet merged to main.
