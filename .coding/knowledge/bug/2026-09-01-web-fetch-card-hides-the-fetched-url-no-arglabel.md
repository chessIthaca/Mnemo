+++
title = "web_fetch card hides the fetched URL (no argLabel case)"
created = "2026-09-01"
status = "superseded"
+++

BUG: web_fetch tool cards in the chat transcript show a bare "web_fetch" header — no URL visible without expanding raw args (backlog 69cf7e9c). Root cause: ToolCard header chips come from buildPathChips + argLabel (frontend/src/components/chat/Message.tsx); web_fetch args {url, max_length} carry no path/file field (no chip) and argLabel had no web_fetch case, so the URL only appeared in the expanded args JSON. Fix: webFetchLabel(toolName, argsJson) helper in frontend/src/lib/toolCardPaths.ts (returns the url, truncated at 60 chars) dispatched from argLabel after the browser dispatch. Regression tests: "webFetchLabel" describe in frontend/src/lib/toolCardPaths.test.ts (incl. argLabel source-contract test).
