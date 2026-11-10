+++
title = "capture Mnemo2 preview scenes at exact window sizes as PNGs with headless Edge"
created = "2026-11-09"
evidence = "Run during the side-panel review on 2026-11-09: seven PNGs produced this way; the Git Bash attempt produced none, and JS widths read 794 at a 1100 window before a screenshot and 660-680 after."
+++

To prove a Mnemo2 layout at real window sizes without touching the live app, run a preview server (preview_start on a launch.json entry for the worktree) and shoot `preview.html?scene=<name>` with headless Edge: `msedge --headless=new --disable-gpu --hide-scrollbars --no-first-run --user-data-dir=<scratch profile> --window-size=W,H --virtual-time-budget=8000 --screenshot=<path>.png <url>`. Run it from PowerShell with `Start-Process -FilePath $edge -ArgumentList $args -Wait -WindowStyle Hidden`, one shot at a time: called from Git Bash the launcher returned at once and wrote no files. The separate --user-data-dir keeps it off the user's real browser. In the built-in browser pane, `resize_window` emulates sizes, but while the pane is hidden the page's resize event lags until a frame paints, so take a screenshot before measuring with JavaScript, or widths read stale.
