+++
title = "expand_result output is archived again, so a large expansion returns another preview"
created = "2026-11-09"
evidence = "`.coding/memory.db` savings_events and tool_result_archive, queried 2026-11-09; turn.rs ingest call has no tool exemption."
+++

Symptom: an agent calls expand_result on an archived result and gets a 2,000-character head+tail preview instead of the content, although the preview promised "Retrieve it all". Root cause: EXPAND_MAX_CHARS is 60,000 (src/tool/agent/expand_result.rs) but every tool result, expand_result's included, passes ingest_tool_output (src/agent/turn.rs), which archives anything over archive_min_chars (20,000). Measured on this project 2026-11-09: 250 of 580 expansions served >=20k chars, and tool_result_archive holds 251 rows with tool='expand_result'; 61 expansions targeted those rows. Fix direction: exempt expand_result from ingestion and keep its own cap and paging. Open at the time of writing.

Amended 2026-11-09: Fixed. ARCHIVE_EXEMPT_TOOLS in src/agent/turn.rs keeps `expand_result`, `plan_read` and `web_fetch` out of the archive; they pass through the ordinary cap. Regression tests: exempt_tools_are_capped_not_archived and other_tools_are_still_archived in src/agent/tests.rs.
