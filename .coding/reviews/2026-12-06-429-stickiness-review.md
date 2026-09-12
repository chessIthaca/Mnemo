## Verdict: FINDINGS (1 high, 1 low)

Review of all uncommitted changes on the working branch (plan 0aeb92cd "Fix: 429 fallback permanently suppresses per-phase model overrides"): the endpoint-stickiness design is sound and correctly restores per-phase model switching on the skill, forced-model, state/subagent-override, and default-provider paths, with clean lock usage and two non-vacuous regression tests. However, the **explicit picker-pin path of `resolve_turn_provider` does not consult `sticky_endpoint`**, so the 429 fallback is now completely broken (and emits contradictory messages) whenever the user has ever used the per-agent model picker — a regression relative to the pre-fix code, which did switch on that path (destructively).

### Finding 1 (HIGH) — 429 fallback is a no-op on the picker-pin path; retry hits the same 429 and the turn fails with a contradictory message

**Location:** `src/agent/loop_impl.rs:865-880` (pin branch of `resolve_turn_provider`), interacting with `src/runtime/agent.rs:312-335` and `src/agent/loop_impl.rs:1280-1316`.

**Reproduction chain (all steps verified in code):**
1. The per-agent picker pin (`set_explicit_provider`, never cleared for the loop's lifetime — doc at loop_impl.rs:1178-1180) makes `resolve_turn_provider` return the pinned provider directly at lines 876-879, **without** calling `sticky_endpoint` — unlike the skill path (line 852), forced-model path (line 894), state/subagent path (line 949), and default path (line 931).
2. The pinned provider 429s. `run_turn_attempt` (agent.rs:312-315) calls `try_429_fallback()`, which **does** run on this path (the pin branch set `resolved_model`/`resolved_provider` at 877-878, so model+endpoint are read correctly): it finds + validates the alternate and records sticky[`model`] = (failed, alt) at loop_impl.rs:1307-1310 — an entry that is then **never consulted**, because the next resolution for that model goes through the pin branch.
3. The caller emits `Rate limited (429) on M@E1 — switching to alternate provider E2` (retrying: true, agent.rs:320-325) and `continue`s.
4. The retry iteration re-enters `resolve_turn_provider` → pin branch returns the **same pinned provider (E1)** → 429 again.
5. `tried_fallback` is now true → `attempt = MAX_PROVIDER_TURN_ATTEMPTS` → final failure whose event text says "rate limited (429), **no alternate provider found**. Switch models via the status-bar picker…" (agent.rs:387-414) — one second after announcing a switch to E2. The turn fails.

**Impact:** the entire 429-fallback feature (plan 6d221559) silently stops working for any agent whose user has used the per-agent picker — the pin persists for the agent's lifetime, so this is every future 429. The pre-fix code did switch on this path (by overwriting the pin — that was the 2026-12-05 bug); the fix removed the pin write without adding stickiness consultation to the pin branch. README.md:59's claim "automatically switches to the same model on a different endpoint if one is configured" is false under a pin. Neither the two new tests nor the four pre-existing 429 tests cover the pin path, so CI stays green.

**Concrete fix (mirrors the forced-model branch at 889-903):** in the pin branch, consult stickiness on a `ModelRef` built from the pin before returning it:

```rust
if let Some((p, cm)) = pinned {
    let pinned_ref = crate::config::ModelRef {
        endpoint: p.provider_name().to_string(),
        model: p.model().to_string(),
    };
    if let Some(alt) = self.sticky_endpoint(&pinned_ref) {
        if let Some(resolver) = self.model_resolver.as_ref() {
            if let Some(built) = resolver.build_turn_provider(&alt, self.fill_rate) {
                self.set_resolved_model(built.as_ref().map(|(q, _)| q.model().to_string()));
                self.set_resolved_provider(built.as_ref().map(|(q, _)| q.provider_name().to_string()));
                return built;
            }
        }
    }
    self.set_resolved_model(Some(p.model().to_string()));   // existing lines
    self.set_resolved_provider(Some(p.provider_name().to_string()));
    return Some((p, cm));
}
```

This keeps the pin's *model choice* intact (endpoint routing only — consistent with the fix's stated design principle), and only reroutes after that exact model 429'd on that exact endpoint. Note the reroute rebuilds via the resolver, so the pin's pre-resolved reasoning effort is not carried into the fallback request — the same tradeoff the forced-model branch already accepts. Also update: (a) the `try_429_fallback` doc path list (loop_impl.rs:1259-1261) to include the picker pin; (b) the `fallback_endpoints` field doc (loop_impl.rs:~205-212) if it claims "every path"; (c) `.coding/knowledge/bug/2026-09-01-429-fallback-pinned-the-model-picker-pin-per-pha.md` ("on every path" claim); and (d) add a regression test: set an explicit pin, 429 on the pinned provider, assert the retry serves the turn from the alternate endpoint (fails on current code).

### Finding 2 (LOW) — smaller-context-window deferral from the old `set_explicit_provider` path is lost; sticky reroute can loop into a persistent context-overflow failure

**Location:** `src/agent/loop_impl.rs:1300-1310` (new `try_429_fallback`) vs the deferral machinery it replaced (`set_explicit_provider`, loop_impl.rs:1194-1212 + `pending_swap`).

The old code routed the fallback through `set_explicit_provider`, which deferred the swap into `pending_swap` whenever the new provider's `max_context` was smaller, so `run_turn` summarized with the old provider first. The new sticky path builds and serves the alternate immediately on the retry iteration with no summarization. Per-endpoint per-model context caps can diverge for the same model id, so if the alternate caps smaller than the current conversation, the retry fails non-retryably (context overflow) — and because the sticky entry persists, every later turn for that model id reroutes to the too-small endpoint and fails the same way until the user intervenes. Edge case (divergent caps for one model id), hence LOW, but it is a real behavior loss versus the pre-fix code and is undocumented.

**Concrete fix:** in `try_429_fallback`, after the validation build (line 1300), compare the alternate's context budget (`cm.max_tokens()` from the built pair) with `self.context_manager().max_tokens()`; if smaller, either (a) feed the existing `pending_swap`/summarize machinery before the sticky retry, or (b) treat it as "no viable alternate" (do not insert the sticky entry, return `None`) so the turn fails once with the actionable message instead of persistently. At minimum, document the loss in the `fallback_endpoints`/`try_429_fallback` doc comments and README.

## Verification notes (everything else checked clean)

### sticky_endpoint coverage — confirmed for the four documented paths
`resolve_turn_provider` consults `sticky_endpoint` at: skill override (loop_impl.rs:852), forced model (894), state/subagent resolver chain (949), and the default-provider path — which correctly synthesizes a `ModelRef` from `self.provider()` (927-930) and would otherwise miss stickiness for the most common configuration; the `default_path_429_fallback_does_not_pin` test exercises exactly this. The pin path is the sole omission (Finding 1).

### Cannot route to the just-failed endpoint / cannot loop — verified
- Entries are written only after `find_alternate_endpoint(&model, &current)` (1296), which excludes the endpoint that just 429'd; `sticky_endpoint`'s own guards (`failed == ref.endpoint && alt.endpoint != ref.endpoint`) prevent both no-op and self-referential entries.
- At most one fallback per `run_turn_attempt` (`tried_fallback`, agent.rs:313); a second 429 goes straight to final failure — no cascade, no backoff burn.
- Cross-turn behavior is bounded self-healing: a later 429 on the alternate overwrites the entry to exclude it; with two endpoints this flips routing back once the original's rate limit clears. No infinite loop is reachable.

### Concurrency — verified clean
- `std::sync::RwLock` with `.expect("… lock poisoned")` matches the file's uniform poisoning style.
- The write lock in `try_429_fallback` (1307-1310) is held only for the `insert`, acquired **after** `build_turn_provider` (1300) and after the `resolved_model`/`effective_provider_name`/`find_alternate_endpoint` reads — no lock is held across a build or any potentially slow call. `sticky_endpoint` takes only a short read lock.
- No code path holds `fallback_endpoints` while acquiring `resolved_model`/`resolved_provider`/`provider`/`explicit_provider` (or vice versa in a conflicting order) — no lock-ordering deadlock.

### Behavior retained — verified
- `FallbackInfo` still returned (1311-1315) and the caller's switching note is unchanged.
- `tried_fallback` respected by the caller; `build_turn_provider` validation constructs a client + `ContextManager` only (side-effect-free); the entry stores a `ModelRef`, not a built provider, so config reloads are picked up at retry-build time. The validation build is then rebuilt once more by the retry — minor wasted work only, not a finding.
- Default-slot swap loss (old `set_explicit_provider` also swapped `provider()`) is harmless: the sticky paths record `set_resolved_model`/`set_resolved_provider`, so UI reporting is correct; `provider()` remains the user-configured default, which is the correct semantics after this fix.

### Tests — non-vacuous and structurally sound; could not execute (no shell in reviewer surface)
My tool surface (read-only by construction) has no shell/exec tool, so `cargo test state_override_survives_429_fallback` / `default_path_429_fallback_does_not_pin` could not be run by me; the plan file records a green full-suite run at step 4. Static assessment:
- **Non-vacuous, both:** with the pre-fix code, turn 1's fallback pins via `set_explicit_provider`; the never-cleared pin then serves turn 2 (immediately, or via `take_pending_swap` completion at the top of turn 2's `run_turn` if the deferral branch fired), so `primary_exec_calls` stays 0 and `assert_eq!(…, 1)` fails. With the fix, turn 2 resolves `exec-model` — a different model id than the sticky one (`plan-model`/`default-model`) — so stickiness correctly does not fire and the Executing override serves the turn. Both tests also assert the fallback endpoint did *not* serve turn 2 (`fallback_*_calls` frozen at the turn-1 count), which is the real anti-pin assertion.
- **State transition is real:** `Workflow::create_plan_with_kind` sets `WorkflowState::Executing` (src/workflow/mod.rs:454); it is called before turn 2's prompt, so turn 2 resolves under Executing.
- **`wait_for_turn_outcome` is robust:** same pattern as the pre-existing `rate_limit_429_falls_back_on_default_provider_path` loop (agent.rs:1724-1749); breaks on `Finished`/`Exited`/channel-close/5s deadline, so a regression fails loudly with a message rather than hanging. A 500ms recv timeout could false-fail on a pathologically loaded machine, but that risk is inherited from the established pattern, not new.
- **Coverage gap:** no test covers the picker-pin path (part of Finding 1's fix).

### Project rules
- **Docs/comments:** new `pub(crate)` field documented (loop_impl.rs:~205-212); `sticky_endpoint` and the rewritten `try_429_fallback` carry thorough doc comments; tests are private. No warning risks spotted statically (no unused imports; `std::collections::HashMap` fully qualified at both initializers, lines 528 and 575).
- **Multi-platform neutrality:** pure platform-neutral Rust (locks, maps, tests); no OS-specific APIs, paths, or shell syntax anywhere in the diff. ✓
- **Documentation sync:** README.md:59 updated and accurate for the covered paths (sticky endpoint routing, overrides keep switching). PLAN.md contains no 429/provider-fallback content (verified — no matches), so nothing stale there. The knowledge bug file and `try_429_fallback`'s path list need the pin-path wording once Finding 1 is fixed.
- **Security:** no `unsafe`, no injection surface; the map is keyed by config-derived model-id strings with no interpolation into commands or paths. ✓
- **`.coding/backlog.jsonl`:** file is exactly 6 physical lines, each a complete valid JSON object with escaped `\n` — the appended design-note item (`86c90ff6`, `[models.reviewing]` reviewer-spawn-only) parses cleanly; the two other appended items and the removed done-item are user bookkeeping, not scope creep. ✓
