# Review — feat/backlog-draft-llm-error-context (2026-05-21)

Scope: ALL uncommitted changes (git diff HEAD) on branch `feat/backlog-draft-llm-error-context`.
Files reviewed: `src/runtime/agent.rs`, `frontend/src/hooks/useAgentStore.ts`,
`frontend/src/components/views/BacklogView.tsx`, `frontend/src/hooks/useAgentStore.test.ts`,
`Cargo.toml`, `.coding/reviews/2026-05-21-memory-system-review.md`, `.coding/plans/*`.

Plan goals: (1) persist Backlog draft across tab switches; (2) push terminal provider
errors into the conversation as an in-context harness note; (3) verify two pre-existing
requests (no code); (4) memory-system adequacy report (docs only).

## Findings

### Correctness — agent.rs harness-note push

- **Push happens exactly once per terminal failure — OK.** The note is pushed only in the
  `else` (retries-exhausted) branch of `run_turn_attempt` (agent.rs:164-199), which runs
  once per call and `return None`s immediately after. `run_turn_with_retry` converts the
  `None` into `AfterTurn::Continue` without re-entering `run_turn_attempt`, so there is no
  loop that could double-push for a single logical failure. A *second* failure (next turn
  also fails terminally) pushes a second note — which is the intended behavior (one note
  per terminal failure).
- **`self.messages` state at push time is sane — OK.** The transient volatile-tail +
  CONTEXT_FOOTER pushes in `turn.rs:457-470` are popped at `turn.rs:530-531` *before* the
  `?` propagates a provider error (comment at 443-448 confirms the pops run even on error
  and are safe because `complete_with_retry` serializes the body before returning the
  stream). So when `run_turn` returns `Err`, the persistent message list no longer contains
  the transient system messages and the note is appended to the real conversation. Note:
  the harness note lands *after* any messages the failed turn already committed (e.g. the
  user's prompt pushed by the `Prompt` arm) — correct ordering.
- **Char truncation is panic-free — OK.** `err_text.chars().take(MAX_PROVIDER_ERROR_NOTE_CHARS)`
  iterates `char`s, never splits a UTF-8 sequence; `collect::<String>()` is safe. The
  2000-char cap bounds context growth. No byte-index slicing anywhere in the new code.

### Store-backed backlog draft — BacklogView.tsx

- **Tab-switch persistence works — OK.** `RightPanel.tsx:114-120` mounts the active view
  via conditional render (`<Body/>`), so leaving the Backlog tab unmounts `BacklogInput`.
  Moving `text`/`attachedImages` from `useState` to the store (`backlogDraft` /
  `backlogDraftImages`, useAgentStore.ts:509-510, setters at 661-662) survives the unmount.
- **Auto-resize effect — OK.** The `useEffect` at BacklogView.tsx:76-81 depends on `[text]`;
  `text` now comes from the store but is still a reactive value via the
  `useAgentStore((s) => s.backlogDraft)` selector, so the effect re-fires on every draft
  change exactly as before. No stale-dependency issue.
- **Stale-closure avoidance in `addImageFiles` — OK and necessary.** `addImageFiles` is a
  `useCallback(..., [])`, so the render-closure `attachedImages` would go stale across
  rapid successive pastes; reading `useAgentStore.getState()` (line 100-101) reads the
  latest state. Correct. Same pattern in `removeImage` (126-127).
- **`handleAdd` clearing — OK.** Clears both fields *before* awaiting `backlogAdd`
  (134-135), so a slow IPC doesn't leave the just-submitted text visible. If `backlogAdd`
  rejects, the draft is lost rather than restored — a pre-existing behavior (the old
  `useState` version also cleared before the await), not a regression introduced by this
  change. Not a finding, noting for completeness.
- **`canAdd` / `handleAdd` guard** (`text.trim() || attachedImages.length > 0`) unchanged
  and still correct against the store-backed values.
- **Store fields are session-scoped (no localStorage) — matches the plan's intent**;
  `resetStore` in the test (useAgentStore.test.ts:31-36) includes the new fields so tests
  don't leak state between cases.

### Test quality

- **`terminal_provider_failure_is_pushed_in_context` is deterministic — OK.** Runs under
  `#[tokio::test(start_paused = true)]`, so both the outer backoff in `run_turn_attempt`
  (`1000 * 2^(attempt-1)` → 1s + 2s) and the inner `complete_with_retry` backoff
  (1s + 2s, dispatch.rs:488-498) auto-advance under the paused clock — no real-time
  dependency, no `tokio::time::advance` needed.
- **Turn-2 proof is real — OK.** The test drives two full terminal-failure cycles and
  asserts (a) turn 1's *first* captured request has no note (guards against a spurious
  early note), and (b) a turn-2 request contains a `Role::User` message whose text has
  both `[harness note]` and the provider's error detail (`provider exploded`). Because
  `AlwaysFailProvider::complete` snapshots the entire `messages` slice per call, this
  genuinely proves the note reached the wire on the next turn.
- **Fanin-loop flake risk — none.** Each loop breaks only on
  `AgentEvent::Error { retrying: false }`. The inner `complete_with_retry` retries do NOT
  emit `AgentEvent::Error` (dispatch.rs is silent on retry; only the outer
  `run_turn_attempt` emits, with `retrying: true` for attempts 1-2 and `retrying: false`
  on the terminal attempt). Mid-stream no-partial-output errors also don't emit
  (turn.rs:800-802 comment confirms the outer layer owns retry messaging). So the only
  `retrying: false` error is the terminal one — the loop can't break early.
  `snaps.len() >= 2` is guaranteed (3 inner attempts/turn × 2 turns = 6 snapshots).
- **Frontend store test** round-trips both setters and the clearing path — adequate
  coverage for a plain value setter.
- **New mock respects `#![deny(warnings)]`** — `AlwaysFailProvider` fields are all read,
  trait methods all used; no `#[allow]` added anywhere in the diff.

### Constitution compliance

- **No `#[allow(...)]` suppressions** added; no `unsafe`. Confirmed by reading the full diff.
- **Doc comments present** on all new public items: `backlogDraft`, `setBacklogDraft`,
  `backlogDraftImages`, `setBacklogDraftImages` (useAgentStore.ts:444-460);
  `MAX_PROVIDER_ERROR_NOTE_CHARS` (agent.rs:23-27); `AlwaysFailProvider` and the new test
  (agent.rs:2075-2085). `BacklogInput`'s doc comment updated to explain the store backing.
- **Warning-free** — reported by the main agent (cargo test 809 passed / zero warnings);
  the code itself shows no unused imports, no dead fields, no unneeded `mut`.
- **Branch** `feat/backlog-draft-llm-error-context` — not main. `.coding/plans/stack.json`
  no longer contains a `merge_to_main` skill payload (it was consumed), so nothing in this
  diff attempts a main merge.
- **Line endings / style** — the Rust and TS additions match surrounding style; existing
  files were edited in place, not rewritten.

### Security — harness note embeds provider error text into the next request

- The note's body comes from `e.to_string()` on `Error::Provider`. The dominant failure
  texts (openai.rs:414 `failed to start stream: {e}`, openai.rs:424-426
  `stream request failed: {status} — {text}`) embed the *response* body / transport error,
  never the `Authorization: Bearer …` header (which is set on the *request* at
  openai.rs:402 and is not echoed by reqwest's error `Display`). So no API key or auth
  material is introduced into the conversation by this change.
- One caveat worth stating: a hostile or buggy provider could in theory return an error
  *body* containing sensitive echoed content, and that body (capped at 2000 chars) would
  then be sent back to the same provider on the next turn. That is a same-origin
  round-trip — the text goes back to where it came from — so it does not cross a trust
  boundary. The main-agent task note mentions redaction lives in the trace layer; that
  layer is unrelated to this path (the note does not go through the trace log). No action
  required; assessed and accepted.

## Verdict

No findings. All five focus areas (correctness of the note push, draft store sync, test
determinism, constitution compliance, secret hygiene) check out. The change is safe to
commit as-is once the closing sequence's test runs confirm green.
