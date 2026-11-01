+++
title = "Mnemo2 tray Quit hung because exit teardown blocked the event-loop thread"
created = "2027-01-11"
+++

Symptom: Quit from Mnemo2's tray hid the window but mnemo2.exe never exited, kept its run file and locked target/release/mnemo2.exe (rebuilds failed until taskkill /F). Root cause: the RunEvent::Exit handler in src-tauri/src/mnemo2/mod.rs ran tauri::async_runtime::block_on(shutdown) on the event-loop (main) thread, and BrowserController::close's webview.close() needs that same thread, so teardown waited on itself forever. Fix: handle RunEvent::ExitRequested instead. Prevent the first exit, run teardown as an async task (spawn_orderly_shutdown), then call app.exit again once an exit-phase flag reads READY. A 10 s deadline and a panicking step both log the pending step and exit anyway. Regression tests in mod.rs: orderly_shutdown_leaves_the_event_loop_thread_free, stalled_shutdown_exits_after_the_deadline, panicking_shutdown_step_still_exits; runtime.rs: shutdown_completes_with_an_open_project_and_team. Rule: never block_on async work from a Tauri run-event callback that may need main-thread services.
