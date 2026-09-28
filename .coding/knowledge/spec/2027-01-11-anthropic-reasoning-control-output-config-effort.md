+++
title = "Anthropic reasoning control — output_config.effort (NOT thinking/budget_tokens; never send a thinking field)"
created = "2027-01-11"
+++

Durable API facts (verified 2027-01-11 against platform.claude.com docs) deciding how Mnemo controls Anthropic reasoning depth:

1) `thinking: {type:"enabled", budget_tokens:N}` (manual extended thinking) is DEPRECATED on Claude 4.6 (still succeeds) and REJECTED with a 400 by Claude 4.7+ ("thinking.type.enabled is not supported"). Never hardcode a budget table.
2) The modern control is the top-level `output_config.effort` (low|medium|high|max|xhigh), NO beta header required; docs: "Where adaptive thinking is available, effort is the recommended way to control thinking depth", and effort "works whether or not thinking is enabled". Wire: {"output_config": {"effort": "medium"}}.
3) Mnemo must NEVER send a `thinking` field: {"type":"disabled"} is a 400 on models whose thinking is always on (Opus 5.5 at every level; Opus 5 at xhigh/max), {"type":"enabled"} is a 400 on 4.7+. OMISSION = the model's own default (thinking already on by default on Opus 5.x / Sonnet 5), so `off` = "send no reasoning control" = pre-effort behavior.
4) Model discovery (/v1/models): entries carry `max_input_tokens` (context window), `max_tokens` (max output), `capabilities.image_input.supported`, and `capabilities.effort.{low,medium,high,max,xhigh}.supported`. Existing Mnemo parser keys (context_length/max_model_len/max_completion_tokens) all miss → caps were always None for anthropic kind.
5) The display ladder is off|minimal|low|medium|high|max → mapped minimal→low (no lower API level); the per-model `reasoning_efforts` allow-list is what gates levels, and endpoint.effective_reasoning_effort_for already resolves override→allow-list-clamp→"max" default, with `off` → None for anthropic kind.

Implemented in plan b3188918 (src/provider/anthropic.rs build_request_json + AnthropicClientConfig.reasoning_effort; src/provider/client_factory.rs threading; src/provider/models.rs caps/effort parsing).

Amended 2027-01-11: Landed (plan b3188918, wt/mnemo; commits 60878df feature, 71c8d9e + 33de125 review fixes; 4 review rounds: 3 low -> 1 low -> 1 low -> PASS).

Implementation: src/provider/anthropic.rs `AnthropicClientConfig.reasoning_effort` -> `build_request_json` emits `output_config.effort` from a const table (minimal->low; low/medium/high/max pass through; off/unset/unknown/blank omit the field entirely — byte-identical to the pre-effort body). src/provider/client_factory.rs threads the resolved effort into `anthropic_client_config` (was dropped: the toolbar/endpoint effort never reached the wire, and the Settings card hid the controls). src/provider/models.rs reads the `/models` Anthropic shape.

INVARIANTS (each was a review finding or a finding's precondition — do not "simplify" them away):
1. No `thinking` field is EVER sent. Both variants 400 on some current generation; omission is the only encoding that travels, and omission IS what `off` means.
2. "Discovery probe set == the UI ladder minus off": `EFFORT_LADDER` (models.rs) = minimal|low|medium|high|max. `xhigh` is deliberately NOT probed — it is not selectable in `REASONING_EFFORTS`, and the value travels verbatim on non-Anthropic kinds, so probing it only produces discoverable-but-unselectable phantom allow-list entries. The Anthropic wire table keeps an xhigh->xhigh entry for hand-edited configs.
3. Discovered allow-lists are emitted HIGHEST-FIRST (merge re-sorts `Reverse(effort_ladder_index)`): the backend clamps an out-of-list value to the list's FIRST entry, so the order is the clamp target.
4. ONE shared frontend resolver, `effectiveReasoningEffort(ep, modelId)` (frontend/src/components/settings/types.ts): per-model value -> endpoint value -> app default "max", and "off" while `supports_reasoning_effort` is off (the backend's FIRST gate). `capsAutofillPatch`, `effortAutofillEligible`, the card's note/hint gates and the autofill guardKey all resolve through it, so the UI can never promise a fill it will not perform.
5. `capsAutofillPatch` autofills a per-model effort allow-list ONLY when it is empty, the discovered list is non-empty, and the effective effort survives it — never an empty list, never a silent clamp-downgrade. A declined state must render an actionable "Apply discovered" hint (rounds 2-3: silence and clamp-claims that cannot occur were both findings); the hint names the resolved effort.

Known residual (documented, deliberate, round-1-accepted; NOT a defect): after a user manually clears a per-model ctx/out field, the caps "auto-fills with these values" note can reappear while the once-per-(model+caps) guard correctly blocks the refill — same note-vs-guard class as finding 5, in the caps path.
