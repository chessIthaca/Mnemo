+++
title = "source-contract tests pin literal source shapes — update pins with refactors; the EOF-newline file-tool freeze"
created = "2027-01-11"
+++

Two operational lessons from the clippy 1.95 sweep (plan efd1f4c3, 2027-01-25 session):

1. SOURCE-CONTRACT TESTS. src-tauri/src/ipc/run_all.rs's forwarder tests (forwarder_signals_compact_completion_for_main_agent, forwarder_tracks_context_usage_per_agent) are include_str!-based: they pin LITERAL source text ("SerializableAgentEvent::Compacted { .. } => {", "compact_in_flight.remove(&agent_id);"). Any refactor that changes pinned text — even a semantics-preserving lint reformat like the collapsible_match guard move — fails the suite (batch 10a failed at run_all.rs:3503). Before committing a refactor near events.rs's forwarder or run_all.rs's dispatch paths: grep the tests module for include_str! pins touching the affected lines; when the shape legitimately changes, update the pin to the new shape PRESERVING the contract intent (the arm exists + is guarded by the pairing removal) and say so in the commit message. Other known pins: the between_items_compact_gate fn_body pins (fill_rate / proxy_cache_ceiling reads — the LiteLLM-removal backlog item #0324749d will need them updated).

2. EOF-NEWLINE FILE-TOOL FREEZE. The file tools cannot express "add only a trailing EOF newline": file_edit single mode strips the trailing newline in its no-op comparison ("old_string equals new_string"); ops mode rejects an identical payload; convert_line_endings no-ops on style-clean files. Sanctioned fallback (used for review L1): PowerShell [IO.File]::ReadAllText + probe CRLF vs LF + [IO.File]::WriteAllText(raw + eol) — one byte, style-preserving, verify via git diff. Root fix queued: the file-tool hardening backlog item (EOF-newline byte-exact comparison + write-path preservation, empty-ops [] fallback, string-JSON arg repair).
