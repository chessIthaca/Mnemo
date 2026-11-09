+++
title = "Trial tool description changes on a real model before shipping them"
created = "2026-11-09"
+++

Shortening tool text is not neutral, and tests only prove pinned wording exists. A first shortening passed every test, yet on DeepSeek it made 8 of 25 runs save unrequested task results with memory_write (0 of 25 before) and raised failed calls from 2 to 6. Adding one sentence to memory_write and two usage hints, plus a parser fix, brought both back to baseline. Method: a temporary example under examples/ runs Engine::builder with config_dir ~/.mnemo and a workspace in a throwaway folder per run, tools limited to Read, Write and Memory (no shell or git), session limits set. Build it from the changed tree and from a temporary worktree at HEAD, run the same tasks on both, record tool_call_start, tool_call_arg_delta, tool_result and usage events, and score from the final files and the answer. Remove the example afterwards. Read the actual calls, not just pass counts: every task passed even when behaviour had got worse. Measured sentences are pinned in the tool schema tests.
