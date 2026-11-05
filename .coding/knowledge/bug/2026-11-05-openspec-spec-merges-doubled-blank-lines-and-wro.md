+++
title = "OpenSpec spec merges doubled blank lines and wrote THEN for extra outcomes"
created = "2026-11-05"
+++

Symptom: closing a plan with spec changes added an extra blank line before every preserved requirement of the capability's spec.md (one more per merge), and a scenario's second outcome was written '- **THEN**' instead of '- **AND**'. Root cause (src/openspec/spec.rs): Spec::parse keeps each requirement's original Markdown up to the next heading, trailing blank line included; Spec::render's reassembly branch added its own blank line before each block. Scenario::render labeled every `then` entry THEN. Fix: reassembly pushes '\n' + block.trim_end() + '\n' for requirements and extra sections (an untouched spec still returns its original bytes); then[0] renders THEN, then[1..] and `and` render AND. Regression tests: modified_delta_keeps_one_blank_line_between_requirements (src/openspec/delta.rs), scenario_renders_second_outcome_as_and (src/openspec/spec.rs).
