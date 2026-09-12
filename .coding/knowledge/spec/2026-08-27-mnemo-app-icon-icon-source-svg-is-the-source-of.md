+++
title = "Mnemo app icon — icon-source.svg is the source of truth"
created = "2026-08-27"
status = "superseded"
+++

SPEC: Mnemo app icon adopted 2026-08-27 (plan 1f47090a, MERGED into main at 5db371d, review PASS) — src-tauri/icons/icon-source.svg is the single source of truth (1024×1024 self-contained SVG: rounded-rect dark bg, M + memory-node mark, #587CFF accent). HOW to change the icon: edit icon-source.svg (keep square viewBox, no external references), then from frontend/ run `npx tauri icon ..\src-tauri\icons\icon-source.svg -o ..\src-tauri\icons` — regenerates every variant in place (PNGs, icon.ico which tauri-build embeds into the Windows resource, icon.icns, Appx Square*/StoreLogo, iOS/Android dirs); tauri.conf.json:22-27 needs no edit (same filenames). Verify with `cd src-tauri; cargo build` (catches a malformed ICO). The in-app Sidebar/About logo is still the lucide Code2 icon — separate surface, unchanged.
