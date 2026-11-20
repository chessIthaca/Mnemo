+++
title = "jsdom 31.0.1 breaks the LeadChat scoped-CSS tests (pin 31.0.0)"
created = "2026-11-19"
evidence = "Commit 634b3b11; the file failed after a fresh npm ci and passed 34/34 with jsdom 31.0.0 (`npm i --no-save jsdom@31.0.0`); after the pin `npm test --workspace mnemo2` exits 0."
+++

mnemo2/src/components/LeadChat.test.ts fails deterministically with jsdom 31.0.1: its CSS parser rejects Svelte's injected scoped styles (~177 "Could not parse CSS stylesheet" warnings), so the two width assertions see 'auto'. The 2.6.0 dependabot lockfile bump moved jsdom to 31.0.1; a checkout with 31.0.0 still installed looks green while a fresh `npm ci` (and CI) is red. Fix: pin `"jsdom": "31.0.0"` exactly in mnemo2/package.json and refresh package-lock.json with `npm install --package-lock-only` (only jsdom's version/resolved/integrity entries move). To prove an unrelated vitest failure is environmental, run the same file in the main checkout or swap only jsdom's version.
