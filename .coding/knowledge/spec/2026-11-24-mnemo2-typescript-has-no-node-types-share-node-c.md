+++
title = "mnemo2 TypeScript has no Node types — share Node code via .mjs + .d.mts"
created = "2026-11-24"
evidence = "Probe test importing node:os failed svelte-check; fixed with a .mjs helper + .d.mts, after which svelte-check reports 0 errors over 4955 files."
+++

mnemo2's TypeScript program (mnemo2/tsconfig.json, `types: ["vite/client"]`) has NO @types/node: `npm ls @types/node` is empty, no lockfile entry, and no .ts file in the repo imports `node:*`. A `node:os`/`node:fs` import in a type-checked mnemo2 .ts file fails `npm run check --workspace mnemo2` ("Cannot find name 'node:os' ... install @types/node"). Node-API code shared by vitest.config.ts or mnemo2 tests must live in plain .mjs with a sibling .d.mts declaration (as scripts/test-tmp.mjs + scripts/test-tmp.d.mts do); the .ts side imports the typed .mjs. Do not add @types/node or ambient node shims without a decision — and note `process` is likewise undeclared in TypeScript there.
