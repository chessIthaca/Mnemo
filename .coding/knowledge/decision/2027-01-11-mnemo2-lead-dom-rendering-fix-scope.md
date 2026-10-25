+++
title = "Mnemo2 lead DOM rendering fix scope"
created = "2027-01-11"
+++

Source review (2027-01-11, C:/Mnemo) supports a frontend-only fix. `mnemo2/src/components/Markdown.svelte` clears `html` before every async render; retain the prior frame and coalesce source updates with one pending animation frame while keeping stale-render protection. `LeadChat.svelte` gates scrolling on entry count and assigns scrollTop unconditionally on additions; use near-bottom user intent plus ResizeObserver/RAF for streaming growth. Native `src-tauri/src/mnemo2/runtime.rs` already forwards real `SerializableAgentEvent` tool lifecycle events; `leadState.ts` drops them and snapshot restore ignores tool calls/results. Add real tool entries, transcript restoration, and an honest host-tool running notice; no Rust IPC or mock activity. Screenshots/CDP remain prohibited for this review.
