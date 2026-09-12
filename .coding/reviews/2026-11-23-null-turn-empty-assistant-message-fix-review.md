# Review — null-turn empty-assistant-message fix

**Scope:** uncommitted changes in `src/agent/turn.rs` + `src/agent/tests.rs` (the
`.coding/plans/*` changes are bookkeeping, ignored per instructions).

**Bug being fixed:** on a "null turn" (model returns `Finish` with no `TextDelta`
and no tool calls), `run_turn` pushed an EMPTY assistant message
(`MessageContent::text("")`) into the persistent history. `validate_request_messages`
(`src/provider/openai.rs:1002-1014`) rejects any assistant message whose content
trims to empty AND has no tool calls. Because the empty message stays in history,
every retry re-sends it and re-fails identically — wedging the session until restart.

**Fix:** (1) root-cause — substitute `"(no output)"` when `text.trim().is_empty()`
at the null-turn push site (`turn.rs:926-930`); (2) defensive — a new
`repair_empty_assistant_messages` free function (`turn.rs:1380-1392`) called once
at the top of `run_turn` (`turn.rs:100`) that rewrites any pre-existing empty
assistant message to `"(no output)"`.

---

## Correctness — root-cause fix is complete for the null-turn case

I enumerated **all four** `role: Role::Assistant` push sites in `turn.rs`:

| Line | Path | Can push empty/whitespace-only assistant msg? |
|------|------|------------------------------------------------|
| 785  | stop-signal (Interrupt/Cancel/Steer during streaming) | **YES — whitespace-only** (see finding below) |
| 891  | malformed-JSON retry | No — `tool_calls` is non-empty here (we're in the `has_bad_json` branch), so the validator's `m.tool_calls.is_empty()` check skips it. |
| 931  | null-turn (no tool calls) | **Fixed** — now substitutes `"(no output)"` when `text.trim().is_empty()`. |
| 950  | tool-calls path | No — `tool_calls` is non-empty (past the `is_empty()` check), validator skips. |

The null-turn site (line 931) is the only site that previously pushed a truly
empty assistant message with no tool calls, and it is now correctly fixed. The
placeholder `"(no output)"` is non-empty, trims non-empty, and carries no special
meaning that any provider would misinterpret — it is a safe placeholder. ✓

The `TurnOutcome.text` returned (line 943) is the original empty `text`, not
`assistant_text`. This is intentional and correct: the user sees nothing was
produced (empty), while the persisted history carries the valid placeholder. The
test asserts `outcome1.text == ""`, matching. ✓

## Correctness — `repair_empty_assistant_messages`

- **`Text` variant:** checks `s.trim().is_empty()` — matches the validator exactly. ✓
- **`Parts` variant:** checks `parts.is_empty()` — matches the validator exactly. ✓
  (A `Parts(vec![Text { text: "" }])` is NOT repaired, but the validator also
  accepts it — the two agree, so no inconsistency. In practice all four push sites
  use `MessageContent::text(...)`, so assistant messages are always `Text`; the
  `Parts` arm is defensive robustness, correctly implemented.)
- **Cannot corrupt valid messages:** an assistant message with non-empty
  `tool_calls` is skipped (`m.tool_calls.is_empty()` is false) — so a legitimate
  assistant turn with empty text + tool calls is untouched. ✓ An assistant
  message with non-empty text is skipped. ✓ Only truly empty assistant messages
  are rewritten.
- **Runs at the right time:** line 100, right after the `Started` event, before
  the loop / system-prompt prepend / provider call. It repairs the caller's
  persistent history before any transient (volatile tail / footer) messages are
  added — and those are `system` role anyway, so they're skipped. ✓
- **Performance:** O(n) linear scan, allocates only when a repair is needed.
  Message counts are hundreds at most. Negligible. ✓
- **No redundant work:** runs at the top of `run_turn`, so it scans pre-existing
  messages only; the current turn's newly-pushed messages are scanned next turn
  (and are already non-empty thanks to the root-cause fix). ✓

## Bugs

### Finding 1 — MEDIUM: line 785 stop-signal path can still push a validator-rejectable assistant message (whitespace-only)

`src/agent/turn.rs:783-791`:

```rust
if stop_reason.is_some() {
    if !text.is_empty() {                       // <-- no trim
        messages.push(Message {
            role: Role::Assistant,
            content: MessageContent::text(text.clone()),
            tool_calls: vec![],
            ...
        });
    }
    ...
}
```

The guard uses `!text.is_empty()` (no trim), but `validate_request_messages`
rejects on `s.trim().is_empty()`. If the model emits only whitespace (a leading
space/newline is common) and the user then Interrupts/Cancels/Steers, `text` is
e.g. `"   "` → `is_empty()` is false → a whitespace-only assistant message with
no tool calls is pushed → the validator rejects it on the next turn
(`"   ".trim().is_empty()` == true).

This is the **same class of bug** the fix targets, at a different push site. It
is pre-existing (not introduced by this diff), and the new
`repair_empty_assistant_messages` backstops it — the next turn's repair trims and
rewrites it to `"(no output)"`, so the session self-heals rather than wedging.
But the root-cause fix at line 926 deliberately uses `text.trim().is_empty()`;
line 785 should mirror that for consistency and to avoid relying on the
backstop. Minimal fix:

```rust
if !text.trim().is_empty() {
```

This skips whitespace-only partial output (same as it already skips truly empty
output), which is the correct semantic for an interrupted turn that produced
nothing meaningful.

### Finding 2 — NONE (test quality note, not a blocker)

`null_turn_does_not_corrupt_history`'s comment claims turn 2 "proves the session
is not wedged by a poisoned history" because the empty message "would be re-sent
and rejected by `validate_request_messages`." The `MockProvider` does not call
`validate_request_messages`, so turn 2 succeeding does not by itself prove a real
provider would accept the history. **However**, the `empty_assistants.is_empty()`
assertion uses the *same predicate* as the validator
(`role == Assistant && tool_calls.is_empty() && content.as_text().trim().is_empty()`),
so it does prove the history passes the assistant-message rule. The test is solid;
only the comment slightly overstates what the turn-2 portion proves. No change
required.

## Security

No security findings. The fix touches only in-process conversation-history
shaping; no new network, auth, file-system, or untrusted-input surface. The
placeholder string is a static literal. The repair function mutates only the
in-memory `messages` slice. ✓

## Constitution compliance

- **Doc comments on public functions:** `repair_empty_assistant_messages` is a
  private (non-`pub`) free function, so no doc comment is *required* — but it has
  a thorough one anyway. ✓ The existing `run_turn` doc comment is unchanged. ✓
- **No `#[allow(...)]`:** none added. ✓
- **Warning-free (by inspection):** `assistant_text` is used (no unused binding);
  the `match` on `MessageContent` is exhaustive (`Text`/`Parts`); the function is
  called once (no dead-code warning); no new imports. The author should confirm
  with `cargo test` (I am read-only and cannot run it).
- **Regression tests present:** two tests added, both proper regression tests
  (fail without the fix, pass with it):
  - `null_turn_does_not_corrupt_history` — bare `Finish` (null turn) then a
    follow-up turn; asserts no empty assistant message remains and turn 2
    returns `"ok"`. Fails without the line-926 fix (empty assistant message
    would be left in history).
  - `repair_recovers_pre_existing_empty_assistant_message` — pre-seeds a
    corrupted history with an empty assistant message; asserts it is repaired
    to a non-empty placeholder. Fails without the repair call at line 100.

## Recommendation

**Approve with one fix:** apply Finding 1 (change `!text.is_empty()` →
`!text.trim().is_empty()` at `turn.rs:783`) so the stop-signal path is
consistent with the root-cause fix and doesn't rely on the repair backstop for
whitespace-only partial output. Everything else is correct, complete, and
constitution-compliant.
