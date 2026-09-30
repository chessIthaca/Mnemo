+++
title = "Mojibake cleanup in image_tools (backlog f4616c13)"
created = "2027-01-11"
+++

Symptom: src/tool/agent/image_tools/ ships double-encoded mojibake in user-visible strings: `â€”` (Windows-1252 round-trip of the em dash —) in tool descriptions, schema strings, error messages, and prompt text, and `â†’` (round-trip of the arrow →) in doc comments and prompt text — 57 source lines across mod.rs, prompts.rs, tools.rs, zoom.rs (verified sweep 2027-01-11), rendered garbled to users and the vision model. · regression test: image_tools_strings_carry_no_mojibake

Full record for plan c4bdb3d4 (see .coding/plans/c4bdb3d4.md for the plan file).

regression test: image_tools_strings_carry_no_mojibake · path .coding/plans/c4bdb3d4.md · branch wt/mnemo @ 0c4826d (unmerged — exists only on this branch)
