+++
title = "a session's pinned task rides the stable system head verbatim and survives compaction"
created = "2026-11-03"
+++

SpawnOptions.pinned_task (src/headless/types.rs) flows through the engine into host SessionOptions, then to AgentLoop::set_pinned_task, stamped post-build and pre-registration. turn.rs renders it under "## Your task (pinned)" in the STABLE head, between the tool groups and the session primer, so it is prefix-cache safe and never in the volatile tail. Compaction summarizes messages[1..cut] only, and the head is rebuilt every request, so the task survives compaction, swaps and switches verbatim. Blank text counts as None, and None adds zero bytes (Mnemo1 unchanged). Subagents never inherit it. It is not persisted in transcripts, so a resume must pass it again. Known gap: a fresh session's first request leaves the head (pinned task included) out of token accounting (turn.rs ~697-790).
