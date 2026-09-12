+++
title = "vendor-policy direction rolled back — main reset to e0ac5dd (2027-01-09)"
created = "2027-01-07"
+++

User decision 2027-01-09: main rolled back to e0ac5dd ("Merge wt/agenticcoding: 429-fallback live-token-count fix + run-all heartbeat stall recovery"), removing the think_tags/vendor-policy work (5361db7, 38d939d, b05e72d). Rationale: vendor-specific complexity kept growing while DeepSeek degraded and GLM then leaked literal think tags (both via the the LiteLLM proxy) — "totally the wrong direction". Stays: GLM stop-boundaries (e20b1df), 429-fallback + run-all heartbeat (e0ac5dd), name-based vendor detection. Reasoning-leak bug OPEN again; next fix direction TBD. origin/main is 174 commits behind (never had the vendor work) — a normal push fast-forwards it to e0ac5dd, no force needed. Recovery: git reflog → b05e72d.
