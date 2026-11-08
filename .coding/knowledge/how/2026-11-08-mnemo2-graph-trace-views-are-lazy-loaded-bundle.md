+++
title = "Mnemo2 Graph/Trace views are lazy-loaded (bundle 941.85 → 838.71 kB)"
created = "2026-11-08"
+++

Mnemo2's Graph and Trace tabs (mnemo2/src/App.svelte) render through Svelte's lazy-component await block — `{#await import("./components/TraceView.svelte") then { default: TraceView }} … {:catch error}` with a `.view-load-error` fallback — so they are no longer statically imported. CostView/MemoryView stay static; vite.config.ts deliberately has no manualChunks (served from local files, so vendor splitting would add files without deferring parse work).

Measured with `npx vite build` in the worktree (outDir target/mnemo2-dist), commit 30cc57ff: single entry chunk was 941.85 kB (gzip 304.19) before; after it is index-*.js 838.71 kB (gzip 273.70), plus GraphView-*.js 42.68 kB + 9.47 kB CSS and TraceView-*.js 65.97 kB + 22.86 kB CSS as separate on-demand chunks. The >500 kB chunk warning remains (markdown/highlight vendors are still static via LeadChat). App.navigation.test.ts already awaited the tab regions via findByRole, so no test edits were needed.
