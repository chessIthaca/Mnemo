+++
title = "LEAD_INSTRUCTIONS sits at Claude Code's 2048-byte limit, so every addition needs an offsetting cut"
created = "2026-11-22"
evidence = "Measured with the long-path helper the test uses; the lead_instructions length test in mcp_server.rs."
+++

src-tauri/src/mnemo2/mcp_server.rs LEAD_INSTRUCTIONS is the MCP initialize `instructions` text, and Claude Code caps it at 2048 bytes. With an 80-character helper path, which appears twice through {helper}, it is 2045 bytes as of 2026-11-22. The test the_lead_instructions_fit_claude_codes_limit_and_name_real_tools enforces `< 2048`. Any new sentence must be paid for by trimming another one. Put detail in the mnemo-lead skill and keep the constant to pointers. Write every bullet with the `\n\` continuation; a raw newline followed by a bare `\` line compiles the same but invites breakage.
