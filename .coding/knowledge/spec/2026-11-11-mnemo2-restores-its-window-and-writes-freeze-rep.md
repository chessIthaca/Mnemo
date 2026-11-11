+++
title = "Mnemo2 restores its window and writes freeze reports"
created = "2026-11-11"
evidence = "window_geometry tests (11) and hang tests; watchdog report_dir_moves; review of src-tauri/src/mnemo2/mod.rs install_shell."
+++

Window: src-tauri/src/mnemo2/window_geometry.rs saves the main window's physical outer position, inner size, outer size and maximized flag to <config_dir>/mnemo2-window.json on every Moved/Resized event (latest-wins saver, tmp+rename; minimised skipped, pre-maximise bounds kept). install_shell builds the window hidden, clamps the saved geometry with Mnemo1's rules (frontend/src/lib/windowRestore.ts: at least 80 logical px overlapping a work area on both axes or it is centred; size between 800x560 logical and the work area), calls set_position BEFORE set_size (a move onto a monitor with another scale makes Windows rescale the window), then shows and re-maximises it. Freeze reports: src-tauri/src/watchdog.rs now lives in the mnemo_app lib (Mnemo1 re-exports it with `pub(crate) use mnemo_app::watchdog`); src-tauri/src/mnemo2/hang.rs starts it in setup with %TEMP%/mnemo2-hang-reports, moves reports to <project>/.coding/logs when the project engine starts (runtime.rs) and stops it when the exit begins.
