+++
title = "Mnemo2 theme — data-theme on html, tokens in app.css, CSP blocks inline pre-paint script"
created = "2026-11-02"
+++

Mnemo2 theming: `mnemo2/src/lib/theme.ts` paints `data-theme` ("light" or "dark") onto `<html>`, sets `color-scheme` and `<meta theme-color>`, mirrors the `[ui] theme` word to `localStorage["mnemo2.theme"]`, and follows `prefers-color-scheme` live while the choice is "system". `mnemo2/src/app.css` declares every colour token once in `:root` (light) and overrides only the flipping ones in `:root[data-theme="dark"]`; components must not carry literal colours (exceptions: Collapsible's alpha-only `#000` mask gradient, and the hue-based avatars, whose lightness comes from `--avatar-bg-lightness` / `--avatar-ink-lightness`).

Key constraint: the Tauri CSP is `script-src 'self'`, so an inline pre-paint script in index.html is BLOCKED. Pre-paint therefore relies on `data-theme="dark"` statically on `<html>` in index.html and preview.html (the UiConfig default theme), plus `applyCachedTheme()` as the first statement of main.ts, then `actions.initTheme()` (a store action at the end of the actions object) reading the authoritative `[ui] theme` through the Appearance transport.

Backend: `src-tauri/src/mnemo2/appearance_settings.rs` accepts `theme` in its patch, normalises it, and refuses unknown words without writing. The preview page honours `?theme=light|dark` and a scene's optional `theme` field (scenes mission-control-dark, lead-chat-dark, graph-dark); preview repaints pass `remember=false` so they never overwrite the saved choice.
