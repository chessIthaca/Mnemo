+++
title = "project root is C:\\Mnemo (agent.md is stale) + browser file:// rules"
created = "2027-01-11"
+++

The shell tool's working directory (project root) is C:\Mnemo — NOT C:\AgenticCoder\AgenticCoder. Evidence: `(Get-Location).Path` -> "C:\Mnemo" and `(Resolve-Path assets/social/day1-card.svg).Path` -> "C:\Mnemo\assets\social\day1-card.svg" (2027-xx, day-1 social card work). Consequence: absolute file:// URLs for the Browser tab / offscreen browser must use file:///C:/Mnemo/... — a C:/AgenticCoder/... URL returns net::ERR_FILE_NOT_FOUND. agent.md's "The project root is C:\AgenticCoder\AgenticCoder" line is STALE and should be corrected. Also: the sandboxed headless (offscreen) browser cannot load file:// at all (chrome-error://chromewebdata) and its viewport is fixed at 800x600 dpr 1, while the app's own Browser tab (browser_navigate) does load file:// when the path is right.
