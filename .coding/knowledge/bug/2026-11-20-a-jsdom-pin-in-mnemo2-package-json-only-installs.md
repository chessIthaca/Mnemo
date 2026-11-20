+++
title = "a jsdom pin in mnemo2/package.json only installs nested; vitest at the root cannot find it on a clean npm ci"
created = "2026-11-20"
evidence = "PR 143 CI run 44641796974 failed with ERR_MODULE_NOT_FOUND; fixed by root package.json overrides \"jsdom\": \"31.0.0\" (merge fc478663); the lockfile entry moved to node_modules/jsdom."
+++

Symptom: CI "Mnemo2 unit tests" failed every file with "Cannot find package 'jsdom' imported from node_modules/vitest/...". Root cause: pinning a dev dependency only in mnemo2/package.json (jsdom 31.0.0) made npm place it at mnemo2/node_modules/jsdom and drop the root entry, while vitest is hoisted to the root node_modules and resolves from there. Local checkouts passed because they still had a stale root copy. Fix: a root package.json "overrides" entry for the pinned version, so it dedupes to node_modules/jsdom. Verify a dependency pin with a CLEAN install (delete node_modules, npm ci) plus `node -e "require.resolve('jsdom')"` from the root, never against an existing node_modules.
