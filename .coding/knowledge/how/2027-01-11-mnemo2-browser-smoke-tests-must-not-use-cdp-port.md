+++
title = "Mnemo2 browser smoke tests must not use CDP port 9222"
created = "2027-01-11"
+++

The running Mnemo desktop app exposes its Browser-tab WebView2 on `--remote-debugging-port=9222`. Launching a separate headless Chrome on that same port or calling `/json/new` against it creates extra Mnemo2 browser instances/tabs with URL bars. For Mnemo2 smoke tests, use the app's own Browser tab (`browser_*` tools) or an isolated headless browser on a different port with `--headless=new`; otherwise close the extra targets and kill the stray Chrome tree. User complaint 2027-01-25.
