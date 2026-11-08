+++
title = "Check a running Mnemo window from a lead session on Windows"
created = "2026-11-08"
+++

Screenshot: PrintWindow(hwnd, dc, 2) from a DPI-aware PowerShell process (SetProcessDpiAwarenessContext(-4)), or the capture is cropped. Input without moving the user's mouse: PostMessage WM_LBUTTONDOWN/UP, WM_CHAR and WM_KEYDOWN to the Chrome_RenderWidgetHostHWND child; coordinates are relative to that child. Traps: parallel Claude sessions share one Mnemo2 window, so dialogs can close and selection can change between clicks; capture first and never click Delete or Stop by coordinates. A child started with Start-Process dies when the tool's shell exits; start it with run_in_background. The desktop app ignores MNEMO_HOME and always uses ~/.mnemo (config_dir_named); only the headless engine honours it, so snapshot and restore ~/.mnemo files it may write.
