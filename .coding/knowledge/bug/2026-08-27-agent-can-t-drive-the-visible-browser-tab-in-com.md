+++
title = "agent can't drive the visible Browser tab in Complete/Planning — ToolFilter hides mutating browser tools"
created = "2026-08-27"
+++

BUG (reproduced live 2026-09-09, session in Complete state): user asks agent to open/navigate the visible Browser tab ("navigate so I can follow what you do") → browser_navigate fails: "tool 'browser_navigate' is not allowed in the current workflow state (Complete)". Read-only browser tools (browser_snapshot/screenshot/console/pages, AutoRun) work fine — CDP attach to the child WebView2 is healthy.

ROOT CAUSE: src/tool/mod.rs ToolFilter::allows — Planning arm (~line 325) and Complete arm (~line 437) have `ToolCategory::Browser => safety == SafetyLevel::AutoRun`, hiding all mutating browser tools (navigate/click/type/eval — all NeedsApproval); Reviewing arm (~line 404) has `Browser => false` (comment says "flip back to true if browser access during review turns out to matter"). The hidden-until-planned rationale copied the "no project mutation without a plan" rule onto browser tools, but browser tools drive the user-visible Browser tab (child WebView2 via CDP, src/browser/mod.rs webview_* methods) — they cannot mutate project files, and their NeedsApproval level already prompts the user per call. Result: in Complete (the state between tasks — when the user most often asks "show me X in the browser") the agent can never navigate the visible tab.

FIX (plan pending): flip the three arms to `Browser => true` (approval gate is the guard, mirroring the Executing arm), update `reviewing_hides_browser_tools` test + add regression that NeedsApproval browser tools are allowed in Planning/Complete.

Note: browser_* tools require the Browser tab's child webview to exist (user opens the tab once); otherwise webview_navigate errors "is the Browser tab open?" — by design, mutations never fall back to the app's own page.
