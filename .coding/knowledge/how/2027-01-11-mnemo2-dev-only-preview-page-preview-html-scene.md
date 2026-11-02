+++
title = "Mnemo2 dev-only preview page (preview.html?scene=…)"
created = "2027-01-11"
+++

Mnemo2 has a dev-only preview page for reviewing UI changes without the Tauri host: `mnemo2/preview.html?scene=<name>` under `npx vite` (binds 127.0.0.1:5180 strict — use 127.0.0.1, not localhost). A missing `?scene=` renders `mission-control` (the default); an unknown name renders an index of scene links. Scenes: mission-control, worker-thread, lead-chat, lead-idle, delete-sheet.

Layout: `mnemo2/src/preview/main.ts` is the router/mount — it imports the same `@fontsource-variable/inter`, `@fontsource-variable/source-serif-4` and `app.css` as `src/main.ts`, builds a scene's `AppState`, calls `app.set(state)`, then `mount(App, ...)` so scenes render the real shell (TopBar, ControlRoom/LeadChat, sheets). Sample state lives in `mnemo2/src/test/previewScenes.ts` (test-side, built on `src/test/fixtures.ts`; per-worker overrides only, fixture values untouched); scene assertions live in `mnemo2/src/preview/previewScenes.test.ts`.

It never ships: `vite.config.ts` has no `build.rollupOptions.input`, so `vite build` emits only `index.html` (preview.html is served in dev only), and `scripts/assert-no-production-mock.mjs` walks only src/lib, src/components, src/App.svelte, src/main.ts — so `src/preview/` and `src/test/` may import the fixtures while `npm run check:no-mock` keeps passing.

Gotchas: an HTML entry with no `<link rel="icon" type="image/svg+xml" href="/favicon.svg">` logs a `/favicon.ico` 404 as a console error; `preview.html` is served from the Vite root and is not type-checked by svelte-check (only .ts/.svelte are).
