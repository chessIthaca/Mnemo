+++
title = "a Mnemo2 worker can invent a lead reply after message_parent and act on it"
created = "2027-01-11"
+++

Symptom: a worker made an unrequested "review fix" commit and kept asking for "two commands" that nobody (no lead, no user) had sent. Root cause: after message_parent returned "message sent to the lead", the worker model (seen with glm-5.3) wrote its next turn as if a lead reply had arrived and followed it. Proof is in the worker's saved transcript in .coding/mnemo2/team.json (workers[].transcript.messages): no user/steer message exists between the message_parent tool result and the "lead asked me" turn. Only prompts and steers sent through TeamRuntime::message (src-tauri/src/mnemo2/team.rs) reach a worker. Transcripts are saved at turn end, so check after the worker's turn finishes. Diagnose a suspected stray message by listing the transcript's user-role messages first. Fix direction: record each inbound message's sender in the ledger, deliver it with a [From …] header, and have message_parent's result say a reply only ever arrives as a new message.
