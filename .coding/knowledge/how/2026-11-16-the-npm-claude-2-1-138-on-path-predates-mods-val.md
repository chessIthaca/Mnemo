+++
title = "the npm `claude` 2.1.138 on PATH predates mods; validate/install of mnemo-workers needs a current CLI"
created = "2026-11-16"
evidence = "2026-11-16: `claude --version` = 2.1.138; `claude plugin validate mnemo2/claude-desktop-mod/mnemo-workers` errors as quoted; the plugin-authoring skill loaded from bundled-skills/2.1.325 describes the format."
+++

On this machine `claude` on PATH is the npm global install (C:\Users\carst\AppData\Roaming\npm\claude.cmd, Claude Code 2.1.138), while Claude Desktop's own engine is 2.1.325. The mnemo-workers mod (mnemo2/claude-desktop-mod/mnemo-workers) uses the mod format: plugin.json with "types": "./types/index.d.ts" and hooks/hooks.json `{ "modules": ["./register.tsx"] }`. The 2.1.138 CLI rejects both (`root: Unrecognized key: "types"`; hooks "expected record, received undefined"), while the plugin-authoring guide of the 2.1.325 engine documents exactly this format and sharing through a `.claude-plugin/marketplace.json` entry. `claude plugin validate .` on the repo marketplace passes. Mnemo2's Connect (src-tauri/src/mnemo2/claude_code.rs) runs the CLI it finds on PATH, so on a machine with an old npm CLI it may fail to install mnemo-workers@mnemo; update the npm package (`npm i -g @anthropic-ai/claude-code`) or put a current CLI first on PATH.
