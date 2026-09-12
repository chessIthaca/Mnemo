+++
title = "Retry malformed tool calls immediately — don't stop to explain"
created = "2026-08-31"
+++

HOW: When a tool call fails with a parameter error (e.g. malformed `files` arg, missing required field), RETRY IMMEDIATELY with corrected parameters — do NOT stop to explain the error first. The system prompt already mandates this ("parse the text, fix the named parameter, re-issue corrected"). Stopping to narrate the mistake wastes a turn and leaves the task half-done. Concrete instance (2026-12-04): a malformed `read_files` call (nested `files` array wrong) stopped the finish↔update_plan deadlock investigation mid-read; the correct call was obvious and should have been re-issued in the same turn.
