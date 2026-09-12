+++
title = "Mnemo app icon — icon-source.svg is the source of truth"
supersedes = "2026-08-27-mnemo-app-icon-icon-source-svg-is-the-source-of"
created = "2026-08-27"
+++

SPEC: Mnemo app icon adopted 2026-08-27 (plan 1f47090a, MERGED into main at 5db371d, review PASS) — src-tauri/icons/icon-source.svg is the single source of truth (1024×1024 self-contained SVG: rounded-rect dark bg, M + memory-node mark, #587CFF accent). HOW to change the icon: edit icon-source.svg (keep square viewBox, no external references), then from frontend/ run `npx tauri icon ..\src-tauri\icons\icon-source.svg -o ..\src-tauri\icons` — regenerates every variant in place (PNGs, icon.ico which tauri-build embeds into the Windows resource, icon.icns, Appx Square*/StoreLogo, iOS/Android dirs); tauri.conf.json:22-27 needs no edit (same filenames). Verify with `cd src-tauri; cargo build` (catches a malformed ICO). The in-app Sidebar About button and About dialog header show the same mark via the inline-SVG React component frontend/src/components/common/MnemoLogo.tsx (adopted 2026-08-27, plan 180ef721; if icon-source.svg changes, mirror the paths/gradients there — its gradient/filter def ids are namespaced per instance with useId so the two coexisting instances don't collide).
