+++
title = "DeepSeek sentinel-mirror (CONTEXT_FOOTER echo) — MITIGATED by fold_volatile_tail, MERGED into main (3a2accd)"
supersedes = "7bb9a608"
created = "2027-01-07"
+++

MERGED into main at 3a2accd (3a2accde38b70ee404ba3cdd1e75f9f0492fb6a4) on 2027-01-11 (merge_to_main skill), branch wt/agenticcoding deleted (pre-merge tip 951ded0). The sentinel-mirror echo trigger is ELIMINATED for DeepSeek-vendor requests: ProviderPolicy::fold_volatile_tail (plan a0eab9ca) folds the volatile tail into the leading system message and omits the CONTEXT_FOOTER entirely — DeepSeek-vendor requests end with the user/tool message, so there are no trailing system blocks to mirror. Historical symptom: DeepSeek (flash tier, thinking mode, via the proxy) opened its first response by mirroring the harness's context footer — a mangled "cache-stable sentinel" echo. The ThinkTagFilter ladder-step fix was PROVEN NOT TO WORK and reverted (that finding stands — do not retry it). Residual (accepted): a text-only suggestion steer still leaves a short system message as the last message on fold vendors (turn.rs Suggestion push site, documented there). If any echo recurs, the R10 any-period repetition guard caps the damage within ~600 tail bytes. Root-cause lineage: the 2027-01-11 exit-note loop repro (47553472) confirmed the echo class from a clean trace.
