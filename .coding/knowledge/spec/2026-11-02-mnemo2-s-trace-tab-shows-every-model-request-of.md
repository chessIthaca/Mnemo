+++
title = "Mnemo2's Trace tab shows every model request of the lead and all workers"
created = "2026-11-02"
+++

The lead's Workbench has a Trace tab (TraceTab.svelte, lib/trace.ts, src-tauri/src/mnemo2/trace_view.rs: list, detail, clear over Engine::trace()). Mnemo2 runs one engine for the whole app (the team runtime opens on the lead's engine handle and worker sessions spawn on it), so the single LlmRequestLog holds the lead's and every worker's requests. Records carry no session attribution, so rows show time, model, endpoint, tokens, duration and status; the list returns summaries only and a detail (request body and response, collapsed with a show-all cap) loads on demand; the tab polls while visible.
