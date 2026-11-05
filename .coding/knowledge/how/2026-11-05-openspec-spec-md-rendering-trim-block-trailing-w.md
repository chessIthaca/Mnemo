+++
title = "OpenSpec spec.md rendering — trim block trailing whitespace; only first outcome is THEN"
created = "2026-11-05"
+++

OpenSpec spec merge spacing: `Requirement::original` (src/openspec/spec.rs) is the requirement's Markdown sliced from its `### Requirement:` heading to the START of the next heading, so it includes the trailing blank line — any reassembly in `Spec::render` must `trim_end()` the block and append exactly one '\n', or each merge adds a blank line before every preserved requirement. The unchanged-document early return must stay (byte-identical round trip). Scenario rendering: only the first `then` entry is THEN; further `then` entries and every explicit `and` entry render as AND, and parse back as `and`.
