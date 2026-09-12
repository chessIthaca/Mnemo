## Verdict: PASS

Review of plan ab6c9829 ("file_edit drift failures steer to a fresh read — no blind retries, gate on the third", bug_fixing, backlog 714196da) — all uncommitted changes on wt/agenticcoder (5 tracked files + `.coding/plans/ab6c9829.md` sidecar). Every claim in the plan verified against the actual code; no findings.

### (a) Marker/gate mechanics — correct

- **Arm-swap (file_edit.rs)**: the nudge (`with_fresh_read_nudge`, file_edit.rs:110-116, returns `Error::NotFound` with the marker interpolated from the imported `EDIT_STALE_READ_MARK` const — single source of truth) is wired at exactly the five drift sites: empty-file arm (:183), past-EOF arm (:192), literal `replaced == 0` miss (:421), fuzzy miss (:471), regex `new_content == content` miss (:524). The literal path's `new_content == content` no-op arm (:426-429) is untouched and stays BARE — verified against the actual arms; it is also unreachable in practice (replaced > 0 with old != new always changes content), so the bare arm is harmless. Schema-mistake errors (identical strings, mode exclusivity) untouched. The regex CRLF retry (prepare_edit_regex :486-492) still matches `Error::NotFound(_)`, so the flipped-ending retry survives the nudge.
- **Array-size safety (steering_stats.rs)**: `MARKERS: [MarkerKind; 10]` (:284-295, order matches the index() arms: ReadNudge=4, LiteralTip=5, KnownMemoryHit=6, ConsolidationDue=7, ShellRedirect=8, EditStaleRead=9); `counts: [Counts; MARKERS.len()]` (:309), `agent_fired`/`agent_switched` HashMap types (:312/:315), `[(0, 0); MARKERS.len()]` init (:359), both `or_insert([0; MARKERS.len()])` sites (:425, :479). Grep for `; 9]` in the file: zero matches — no runtime-panic hazard remains.
- **detect tool-scoping** (:253-255): `tool_name == "file_edit" && first_line().contains(self.marker())` — a read tool's file content echoing the phrase can never fire. The nudge is a single line (the `\`-continuation format string strips newlines), and the tool passes the error through verbatim (`ToolResult::error(format!("{e}"))`, file_edit.rs:628), so the marker is on the result's first line in production — the detect surface is real, not just test-shaped.
- **Gate no-feedback**: the interception (dispatch.rs:280-289) returns `(redirect, Vec::new())` before the safety/approval flow (:291) and before `observe_call` (:461) / `observe_result` (:468) — the intercepted call never runs, never feeds its own marker count, and the queued C3 note survives for the read that lifts the gate. The interception message itself cannot fire any marker even if it were observed: "re-read the file" (lowercase) ≠ the marker "Re-read the file" (capital R, case-sensitive contains), and no other marker substring appears.
- **Per-agent isolation**: `should_gate_file_edit` (steering_stats.rs:529-543) reads `agent_fired[agent_id][9]` / `agent_switched[agent_id][9]` — per-agent by construction; test `should_gate_file_edit_is_per_agent` verifies agent 2 is unaffected.
- **Path extraction**: `parsed_call.arguments.get("path").and_then(|v| v.as_str())` matches the schema field `path: String` (file_edit.rs:31); a non-string/absent path degrades to `"<the edited file>"` in the message.

### (b) User directive mapping — remedy is sufficient

- "don't edit without read": the gate intercepts the third blind attempt and every subsequent blind attempt (fired stays 2 — the interception never re-fires), so no blind edit can slip through until a read happens.
- "not timeout after three failures": the endless blind-retry loop is broken — the interception message is actionable and names the exact path (`First re-read the file (read_files path="…")`), and the gate lifts on the first read (`observe_call` counts read_files/search_read as the switch, steering_stats.rs:472-474). The message's `{fired}` count ("your last 2 file_edit attempts failed") is accurate at the third call.
- Design note (not a finding): once armed the gate intercepts ANY file_edit, including edits of a different file — but the message names the actual call's path, so the remedy is always correct; this mirrors the C5 symbol gate's over-blocking and is documented in the plan.

### (c) Regression test quality — all five fail without the fix

- `drift_class_errors_carry_fresh_read_nudge` (file_edit.rs:909-937): all five drift paths assert the marker via the imported const; without the nudge the errors are bare → all five asserts fail.
- `edit_stale_read_fires_on_failed_file_edit_only` (steering_stats.rs:736-744): without the kind, `count_of` returns `(u64::MAX, u64::MAX)` → fails loudly; also proves tool-scoping (read_files echoing the marker never fires).
- `should_gate_file_edit_arms_after_threshold_and_lifts_after_a_read` (:747-757) and `should_gate_file_edit_is_per_agent` (:760-766): without the fn → compile error.
- `file_edit_redirect_intercepts_after_threshold_and_lifts_after_a_read` (dispatch.rs:826-849): without the fn → compile error; asserts both the interception and the lift-on-read.
- The `EDIT_STALE_ERR` test const differs from the live nudge text only in "has drifted" vs "has likely drifted" — intentional; the tests assert the marker contract (substring), not byte-identity.

### (d) Multi-platform neutrality — clean

No Windows-only APIs, paths, or shell syntax. The format strings are standard Rust; the path is only interpolated into a message string — no path manipulation, no filesystem access in the new code.

### (e) Documentation sync — accurate and complete

README.md:53 steering paragraph documents the drift-class nudge, the `edit-stale-read` marker, the read-tools-as-switches, and the two-failures-then-intercept gate with lift-on-read — matches the implementation. The steering module doc (steering_stats.rs:7-13) lists the fresh-read note and both C5-family gates. Doc comments on `with_fresh_read_nudge`, `EDIT_STALE_READ_MARK` (with the output-contract note), `MarkerKind::EditStaleRead` (all five arms), `should_gate_file_edit`, and `file_edit_redirect` are accurate. The frontend renders `by_marker` dynamically (LlmTraceView.tsx:976, types.ts:419) — no hardcoded kind list, so the 10th kind flows through the snapshot.

### (f) Scope — clean

Diff touches only `.coding/backlog.jsonl` (status → in_flight with plan-id note, expected bookkeeping), README.md, dispatch.rs, steering_stats.rs, file_edit.rs; the sole untracked file is the plan sidecar `.coding/plans/ab6c9829.md`. No unrelated changes.

### (g) Security — no injection surface

The path from call args is interpolated via a plain `{}` placeholder with a string argument — no user-controlled format specifiers, no format-string weirdness. The result is a text node in the tool result displayed to the model; nothing is executed or parsed. The `path.unwrap_or("<the edited file>")` fallback covers malformed args.

### Observations (non-findings)

1. The C3 escalation note for `edit-stale-read` attaches to the read result (after the switch already happened), so its "follow it on the next call" wording is slightly stale at that point — pre-existing C3 behavior, harmless, not introduced here.
2. The gate's over-blocking of edits to other files (see (b)) is a deliberate, documented tradeoff mirroring C5.
