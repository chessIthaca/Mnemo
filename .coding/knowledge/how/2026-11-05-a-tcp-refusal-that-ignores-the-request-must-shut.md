+++
title = "a TCP refusal that ignores the request must shutdown-write then drain-read, or macOS clients get ConnectionReset"
created = "2026-11-05"
+++

On BSD sockets (macOS), closing a TCP socket while received data is still unread sends an RST instead of a FIN, so the peer's read fails with `ConnectionReset` even when the server already wrote a response. Consequence: a server-side refusal that does not consume the client's request (e.g. an HTTP 503 "too many connections" reply written and then dropped) is only correct on Windows; on macOS the client never sees the response. Correct close sequence: write the response, shut down the write half (`AsyncWriteExt::shutdown`), then drain the read side until EOF under a short timeout (500 ms is enough), ignoring errors — the close becomes an orderly FIN on every platform. Verify per-connection handlers are spawned (not awaited inline in the accept loop) before adding the drain, or the drain delays new accepts. Reference implementation: `reject_busy` in src-tauri/src/mnemo2/mcp_server.rs (Mnemo2), whose regression test `connections_over_the_cap_get_a_503_and_the_slots_come_back` is macOS-only red.
