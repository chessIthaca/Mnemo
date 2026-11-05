+++
title = "What closing the Mnemo2 window does is a setting; a hide to the tray is announced once per run"
created = "2026-11-03"
+++

src-tauri/src/mnemo2/window_settings.rs: `[window] close = auto | tray | quit` in <config_dir>/mnemo2.toml (default auto), edited in Settings > Appearance ('When I close the window').
- auto: hide to the tray when started with --hidden (as the Claude Code plugin helper starts it) and a tray exists, else close.
- tray: always hide when there is a tray.
- quit: request_quit, which still asks while workers run.
The behaviour is read live on every CloseRequested. The first hide-to-tray of a run sends an OS notification ('Mnemo2 is still running'). Note: any Mnemo MCP tool call auto-starts a hidden Mnemo2 if none runs, so a rebuild must avoid Mnemo calls until the build finishes.
