+++
title = "DeepSeek reasoning tokens in main chat — OPEN (fix rolled back; GLM-5.3 no longer affected)"
supersedes = "77278715"
created = "2027-01-07"
+++

OPEN on main (e0ac5dd) as of 2027-01-09; scope corrected 2027-01-10 (user-verified): DeepSeek-only — deepseek-v4-flash-gcp/aws via the the LiteLLM proxy (<gateway>); GLM-5.3 is no longer affected (works flawlessly — see the second amendment). Symptom: reasoning tokens from proxy-served models render in the MAIN CHAT WINDOW as assistant text. Root cause: the proxy emits reasoning inline in content instead of reasoning_content; at e0ac5dd the ThinkTagFilter is gated to Local-kind endpoints only (src/provider/openai.rs, commit 949fed2's gate), so proxied think-tag text streams as TextDelta into the transcript. The fix (plan 77278715, commit 5361db7, merged b05e72d) was ROLLED BACK 2027-01-09 — the config-knob direction was rejected by the user. Next direction TBD.

Amended 2027-01-07: 2027-01-10 USER CORRECTION (authoritative): the solutions tried for this leak are PROVEN NOT TO WORK — twice: (a) the vendor-policy per-model think_tags fix (plan 77278715) rolled back 2027-01-09 with the leak persisting, and (b) the reasoning STILL leaked into the main window with the sentinel fix live at 5cdd822 (the merge-5cdd822 rollback plan's live-check note). The bug is REAL (deepseek-v4-flash-gcp/aws AND glm-5.3, both via the proxy); the tag-based solutions are NOT real solutions. The pending stream-level ThinkTagFilter heuristic (backlog 0d4f54e3) is the same approach a third time and must NOT be prescribed as the fix; the leak's solution needs fresh investigation (the proxy's stream parsing / serving stack), not another render-level attempt.

Amended 2027-01-07: 2027-01-10 USER CORRECTION (follow-up, supersedes the scope above): the bug is NO LONGER REAL for GLM-5.3 — GLM works flawlessly now (user-verified live 2027-01-10). The leak is DeepSeek-only: deepseek-v4-flash-gcp/aws via the proxy. The "AND glm-5.3" scope in this record's earlier text is stale — do not treat GLM as affected when investigating or fixing; any fix direction must be validated against DeepSeek serving (the the GCP vLLM-SGLang proxy), not GLM.
