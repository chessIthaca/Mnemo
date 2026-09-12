+++
title = "@tailwindcss/typography is required for markdown prose/heading styling"
created = "2026-08-24"
+++

SPEC: Markdown prose styling requires @tailwindcss/typography (installed 2026-08-24, merged to main at dd660698).

The app renders markdown via react-markdown (parses `# Header` → `<h1>`) wrapped in `prose prose-invert prose-sm` containers across 5 surfaces: chat (Message.tsx:214), file view + edit preview + code preview (SourceEditor.tsx:513/519/525), and plan progress (PlanProgress.tsx:229). Those `prose` classes come from the @tailwindcss/typography Tailwind plugin — WITHOUT it, `prose` generates zero CSS and Tailwind Preflight strips headings to font-size/weight: inherit (headers render as plain text). The hand-written `.prose` rules in globals.css (lines 230-248) cover tables/lists/links/blockquotes but NOT headings.

Fix: `@tailwindcss/typography` ^0.5.19 (MIT, devDependency) installed + registered in frontend/tailwind.config.ts (`plugins: [typography]`, was `plugins: []`). The `.prose` rules in globals.css layer ON TOP of the plugin and must remain (e.g. `.prose { font-size: inherit }` overrides prose-sm's 0.875rem so the user's --app-font-size applies). Credited in the About dialog's "Frontend — build & dev" group (dependencies.ts).
