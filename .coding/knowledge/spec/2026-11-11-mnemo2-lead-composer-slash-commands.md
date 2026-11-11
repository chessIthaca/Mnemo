+++
title = "Mnemo2 lead composer slash commands"
created = "2026-11-11"
evidence = "Commits 48de07a7 (lead_new) and cffc598f + fa6a394c (composer). Tests are in mnemo2/src/lib/slash.test.ts and mnemo2/src/components/Composer.test.ts, plus new_conversation_* in src-tauri/src/mnemo2/runtime/tests.rs."
+++

The lead composer (mnemo2/src/components/Composer.svelte, parser in mnemo2/src/lib/slash.ts) runs /new and /clear (same action: host lead_new = LeadRuntime::new_conversation, a fresh session on the same model with an empty transcript persisted to lead.json), /compact, /model <name> [effort] (exact spec → bare model id → unique substring), /stop and /help.

Rules:
- A draft is a command only when its first token is a known name. Any other /text is sent to the lead as a normal message.
- In the suggestion menu, Enter RUNS a command only when its name is typed out in full. On a partial name Enter completes it, like Tab, so "/c" + Enter can never clear the conversation. A mouse click runs the command.
- Commands bypass the pending-question and send gates. Freeform-answer mode never parses commands.
- Worker reply boxes have no commands.
