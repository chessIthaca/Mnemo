+++
title = "one-decimal percentage display rule — fmtPct everywhere % is text"
created = "2026-08-26"
+++

All user-facing percentage text in Mnemo renders with at most one decimal digit via fmtPct (frontend/src/lib/format.ts): Math.round(x*10)/10 + toLocaleString("en-US", {maximumFractionDigits:1}), zero-guard (never "-0"/trailing ".0"), integers stay integral ("55%", "99.8%", "100%"). Convention: any new % TEXT site must render `${fmtPct(x)}%`; bar-width/geometry styles (style={{width}}) are exempt (not displayed numbers). Numeric producers cacheHitPct / tokenTipRows cached share / traceSummary.avgCacheHitPct (frontend/src/lib/traceStats.ts) all round to 1 decimal and clamp to 100. Regression tests: frontend/src/lib/format.test.ts (fmtPct describe) + frontend/src/lib/traceStats.test.ts. Commit 024ccef on wt/toolcard-chips-salvage-review (plan c195f939, backlog 9042b47c); review .coding/reviews/2026-12-fmt-pct-one-decimal-review.md PASS.
