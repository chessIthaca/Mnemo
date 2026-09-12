+++
title = "backlog_add nice title display (backlog fef458f1)"
created = "2026-12-21"
+++

Backlog item fef458f1-a1cc-48bd-8bce-e950465dea1a: "backlog_add should have a nice display so of the backlog title."

INTERPRETATION (confirmed by code exploration): the chat ToolCard for the `backlog_add` tool shows a bare `backlog_add` header with the item's text hidden behind expand. The "backlog title" = the tool's `text` argument. Fix = add a `backlog_add` branch to `argLabel` (frontend/src/components/chat/Message.tsx:438-616) that shows the first line of `text`, truncated + quoted, mirroring `search`'s quoted-pattern chip (Message.tsx:576-581). Frontend-only; the Rust tool output (`added backlog item #… (pending): preview`) is already a fine one-liner.

Related backlog 8c1d8a47 ("json input/output of commands not useful for humans → readable content") is the same theme (nicer tool-card labels).

Mechanics: `argLabel(args, toolName)` dispatches per-tool; `displayName(name)` maps raw→friendly (only spawn_agent + graph_* today — keep raw `backlog_add` for consistency with shell/git/search). `argPaths` (toolCardPaths.ts:51-68) excludes label-only tools from file-link salvage — backlog_add belongs there. Tests: messageArgLabel.test.ts (per-tool describe blocks).
