# Review: Local system-message sanitizer (2026-04-09)

**Scope reviewed:** all uncommitted changes (`git diff HEAD`):
- `src/provider/openai.rs` (+222): `sanitize_local_messages` free fn, hook in
  `OpenAiClient::build_request_json`, `use std::borrow::Cow;`, 3 unit tests +
  `local_test_client()` helper.
- `.coding/plans/stack.json` (plan bookkeeping — plan id swap + skill payload
  removal; benign, expected session state churn).

Cross-checked against: `src/provider/mod.rs` (Message/MessageContent/as_text),
`src/agent/context.rs` (summarize paths), `src/agent/turn.rs` (head insert at
404, volatile-tail/CONTEXT_FOOTER pushes at 456–471, suggestion push at 208),
`src/runtime/agent.rs` (suggestion push at 469), `src/provider/vision.rs`
(own request builder).

## Correctness — verified (no findings)

- **Choke point is the right one.** `OpenAiClient::complete`
  (`src/provider/openai.rs:371`) is the only streaming path; it calls
  `build_request_json` (`:380`), and the sanitizer runs before serialization
  (`:678-682`). The only other POST to `/chat/completions` is
  `VisionClient::describe_image` (`src/provider/vision.rs:139-198`), which
  builds its own body containing a single `role: "user"` multipart message —
  no `system` message ever, so it cannot trigger the Jinja failure and
  correctly needs no treatment. **Out of scope, confirmed.**
- **Producers confirmed.** Non-leading system messages are genuinely reachable:
  summary lands at index 1 in both `summarize` (`context.rs:183-196`) and
  `summarize_with_interrupt` (`context.rs:301-314`); suggestion re-injection
  pushes a trailing system message (`turn.rs:208-216`, `runtime/agent.rs:469-475`).
  The volatile-tail + CONTEXT_FOOTER pushes are already guarded behind
  `!is_local` (`turn.rs:456`). The doc comment on `sanitize_local_messages`
  mentions the volatile tail as a producer "on the OpenAI path" — slightly
  imprecise (those pushes never fire for Local), but it says exactly that, so
  it is not misleading.
- **Empty list:** `skip(1).any()` → false → `Cow::Borrowed` → serializes to
  `[]`. Fine.
- **Single system at index 0:** fast path, untouched. Fine.
- **System at 0 with later systems:** only indices > 0 demoted; leading system
  preserved. Matches tests.
- **Multipart content:** `as_text()` concatenates text parts with `""` and
  silently drops `ImageUrl` parts (`mod.rs:161-173`). Not a real-world path:
  every producer that creates a system message uses `MessageContent::text`
  (context.rs:185, turn.rs:210/459/466, runtime/agent.rs:471), and a system
  message with an image block makes no sense. Even if one appeared, text is
  preserved and the demotion still achieves the goal (killing the 500).
- **Fast path / Cow:** the borrow propagates correctly — both branches bind a
  `Cow<'_, [Message]>` (elided to the `messages` param lifetime), and the
  serializing loop's `.iter()` auto-derefs through `Cow`. The OpenAI branch's
  `Cow::Borrowed(messages)` matches the sanitizer's no-op return type. Tests
  verify behavior through the public JSON output, which is the contract that
  matters.
- **OpenAI passthrough:** `ProviderKind::OpenAI` never enters the sanitizer;
  trailing system preserved byte-for-byte (test at `:2070-2107`).
- **No information loss:** demotion rewrites content to `Text("System: " +
  as_text())`. For all real producers the original content is already `Text`,
  so `as_text()` is lossless. The `System: ` prefix preserves provenance.
- **Trace capture:** `log.start(...)` is called with the already-sanitized
  body (`openai.rs:392`), so traces reflect what was actually sent. Good.

## Findings

### Low

**L1 — Demotion can produce consecutive `user` messages, including a `user`
immediately after the leading `system` — worth a comment, not a blocker.**
`src/provider/openai.rs:838-855`.

The most common trigger is the summary at index 1 (`context.rs:195`): the
sequence is `[System, System(summary), User, ...]`, which demotes to
`[System, User("System: ## Conversation summary..."), User, ...]` — two
consecutive `user` messages. The suggestion push (`turn.rs:208`) can likewise
append a demoted `user` right after a trailing assistant or tool message.

The buggy template ("System message must be at the beginning") is fixed
regardless — that check is keyed on `role == "system"` position, not on
alternation, so consecutive users do not re-trigger it. Strict
user/assistant alternation is also **not** a universal requirement: LM
Studio's default chatml and the llama.cpp/Mistral-family Jinja templates do
not enforce it (and this app already targets those same endpoints through
Ollama, where consecutive users work). Some templates (a subset of
Gemma-style ones) dislike a leading `system` at all or prefer strict
alternation, but that failure mode predates this change and is unrelated to
it. Severity: Low — the pragmatic choice is correct; suggest one sentence in
the doc comment noting that demotion may yield consecutive `user` messages
and that this is accepted as the lesser evil vs. a hard 500.

**L2 — `.coding/plans/stack.json` diff is session-state churn, not a code
change.** It swaps the active plan id and drops a completed `merge_to_main`
skill payload. Expected bookkeeping; no action. Flagged only so the committer
knows it is intentionally included.

## Security

No findings. The sanitizer only rewrites roles/content of messages already
destined for the model; it introduces no new network paths, no secrets
handling, and no injection surface (the `"System: "` prefix is plain text in
content the endpoint already receives).

## Constitution compliance

- **Warning-free / no suppressions:** `search` for `#\[allow` in
  `src/provider/openai.rs` → **no matches**. No `#[allow(...)]` added. PASS.
- **Doc comments:** `sanitize_local_messages` is a private free fn and has a
  thorough doc comment (`openai.rs:809-824`). No public API was added.
  "All public functions must have doc comments" — satisfied. PASS.
- **`cargo test`:** reported green (789 lib + integration, zero warnings
  under `#![deny(warnings)]`). Reviewer is read-only; trusted per the task
  brief, and the test-output file `test_output.txt` in the tree corroborates
  the pre-existing suite.
- **Line endings / style:** diff is consistent with the file's existing style
  (targeted `file_edit`-style additions, no rewrite). PASS.

## Verdict

**No blocking findings.** Two Low items: (L1) add a one-line comment noting
consecutive-`user` demotion is accepted; (L2) stack.json churn is expected.
Ship it.
