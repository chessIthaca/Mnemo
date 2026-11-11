+++
title = "Mnemo1 retires as is; only the chat slash commands come over to Mnemo2"
created = "2026-11-11"
evidence = "The user's own words in chat on 2026-11-11: \"no we can skip those and retire mnemo1 as is\", then \"but do introduce the /commands\". The feature comparison is in the parity check of the same day."
+++

The user decided on 2026-11-11 to retire Mnemo1 (the React app in frontend/ and its Tauri bridge in src-tauri/src/ipc/) as is.

ONE Mnemo1 feature still moves to Mnemo2: the chat slash commands in the composer (Mnemo1's InputBar).

NOT ported, deliberately; they retire with Mnemo1:
- the approval card's "approve and always allow" buttons (add_safety_rule, add_safety_rule_broad, add_safety_rule_class); rules stay editable on Settings → Safety;
- /save and /load of the chat to .coding/conversations/ (durable session context is still the separate 2.4 backlog item);
- the Stats tab's per-session token list (Mnemo2 shows cost per model and savings);
- Laya's automatic startup fine-tune ([general.laya] auto_finetune, src-tauri/src/ipc/finetune.rs);
- everything already listed in "DECISION: Mnemo1-only settings — which retire, which stay config-only, which move to Mnemo2".

The v2.3.0 tag may still be cut from main before the removal lands.
