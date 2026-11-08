+++
title = "A '# ' heading inside a plan section replaces the plan's title"
created = "2026-11-08"
+++

Symptom: plan_update with sections.research = "# Findings ..." made a plan titled "Live check only..." show as "Findings" on the board and in .coding/plans/<id>.md line 1 (`# Plan: Findings`). Cause: the plan file parser treats `# ` and `## ` lines as structure (see the comment on lift_acceptance in src/app/plan_board.rs), and custom schema sections are written into the file unescaped. Only custom schemas hit it, and their templates usually start with a `# ` heading, so filling in a template as written triggers it. Fixed 2026-11-08: render_schema_sections in src/app/workflow/plan_file.rs escapes these lines with escape_section_markers at render time; board.json keeps the raw text, so the archive is unescaped. Regression test: a_section_heading_does_not_rename_the_plan in src/app/plan_board.rs. Confirmed in the rebuilt app.
