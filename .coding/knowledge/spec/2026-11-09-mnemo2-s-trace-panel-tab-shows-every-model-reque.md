+++
title = "Mnemo2's Trace panel tab shows every model request of the lead and all workers"
supersedes = "2026-11-02-mnemo2-s-trace-tab-shows-every-model-request-of"
created = "2026-11-09"
evidence = "Workbench.test.ts 'hands openTrace filters to the panel's Trace tab'; TraceView.test.ts 'shows one column in a narrow panel and two in a wide one'; measured at a 1100x720 window: list 420px and reachable."
+++

Trace is a tab of Mnemo2's side panel hosting the full TraceView.svelte, lazy-loaded (lib/trace.ts, src-tauri/src/mnemo2/trace_view.rs over Engine::trace()). The compact TraceTab and the top-level Trace route are gone: a smaller Trace beside the full one in the same panel had no purpose. Mnemo2 runs one engine for the whole app (the team runtime opens on the lead's engine handle and worker sessions spawn on it), so the single request log holds the lead's and every worker's requests. The list returns summaries and a detail loads on demand; the view polls while visible. actions.openTrace(filters) opens the tab from any main tab without changing the route, and the view consumes the hand-over once, whether or not it was already mounted (Model calls on a worker passes `{ agent }`). The view measures itself: under 760px it shows the list or one call instead of two columns. It scrolls vertically and the call list has a 420px basis, because the charts and filters could otherwise leave the list 0px tall behind the panel's clipping.
