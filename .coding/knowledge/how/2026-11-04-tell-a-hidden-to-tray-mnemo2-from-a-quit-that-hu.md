+++
title = "Tell a hidden-to-tray Mnemo2 from a quit that hung"
created = "2026-11-04"
+++

Closing the Mnemo2 window on the Automatic close behaviour (the default) hides it to the tray, and since the close-behaviour change it shows a one-time "keeps running in the tray" notice. A hidden Mnemo2 keeps its run file, keeps answering its endpoint and has an empty MainWindowTitle, which looks exactly like a hung quit. Real quits: the tray menu's Quit (right-click the icon) or `mnemo2-mcp quit` / `npm run rebuild:mnemo2`. Verified 2026-11-04: both exit in about 2 s with Laya and the run file gone. An earlier "tray Quit did not exit" report the same day was almost certainly a window close.
