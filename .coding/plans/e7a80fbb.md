# Plan: Day-1 social post: shareable card + paste-ready text

## Goal
Add a 1200×630 social-media card (assets/social/day1-card.svg) and a paste-ready Day-1 launch post with alt text and PNG export notes (docs/marketing/day1-post.md), and render a PNG of the card for direct sharing if the headless browser can load the SVG.

## Kind
implementation

## Context
Findings: assets/workflow.svg exists (read in full) — light theme, cyan #0891b2 / #0e7490 accents, slate #64748b/#94a3b8 secondary text, ui-sans-serif font stack; the new card reuses the cyan accent on a dark navy (#0f172a) background for social contrast. docs/ currently holds only CONFIGURATION.md and FEATURES.md (listed) — docs/marketing/ is new. README.md (read) supplies the verified claims used on the card and post: four-tier memory, tree-sitter graph over 12 languages, six [general.optimizer] levers on by default with a Savings dashboard, per-state multi-model routing, read-only reviewer, git merge/push always gated, credential redaction in output compression. Post text and card layout are drafted in the chat message preceding this plan. Most social platforms reject SVG uploads, so a PNG render is attempted via the browser tool group (load_tools browser) — assumption: the headless browser can open a local file URL; if not, the md file documents manual export (open SVG in a browser → screenshot at 1200×630, or Inkscape/`npx svgexport`). Verification: `cargo test` is unaffected (no Rust changes) — sanity-check the SVG renders by opening it; confirm the md reads cleanly.

## Steps
- [x] 1. **Write the social card SVG** — create assets/social/day1-card.svg, viewBox 0 0 1200 630, dark navy #0f172a background, cyan #0891b2 accents matching assets/workflow.svg. Content: 'Mnemo' wordmark + 'An agentic coding harness that remembers.' top-left; headline 'Stop paying flagship prices for an agent that forgets.'; three rounded boxes with arrows: 'Sharp model / PLANS' → 'Cheap model / EXECUTES' → 'Read-only reviewer / AUDITS'; four pills: '4-tier memory · survives sessions', 'tree-sitter graph · 12 languages', '6 token levers · live savings dashboard', 'no edits without a plan · git push always gated'; footer 'github.com/chessIthaca/Mnemo · Rust + Tauri · Windows + macOS · MIT'. Include <title>/<desc> for accessibility. LF line endings.
- [x] 2. **Write the paste-ready post** — create docs/marketing/day1-post.md containing: the Day-1 post text exactly as drafted in chat (hook, five emoji bullets, platform line, repo URL, 'one lever a week' follow CTA), a short-form variant under 300 chars, suggested hashtags (#AIcoding #Rust #OpenSource #LLM #DevTools), the image alt text, a pointer to assets/social/day1-card.svg, and PNG export instructions (browser screenshot at 1200×630, Inkscape, or `npx svgexport`). Mention the planned series (#1 delta reads … #5 memory) in one line.
- [x] 3. **Render PNG via headless browser (best effort)** — load_tools browser; navigate to the local assets/social/day1-card.svg (file URL or the dev server if file URLs are refused), set viewport 1200×630, screenshot to assets/social/day1-card.png. If the browser cannot load it, skip the PNG and leave the manual export notes in docs/marketing/day1-post.md as the sanctioned path; note the outcome in the plan.
- [x] 4. **Verify and commit** — open assets/social/day1-card.svg and docs/marketing/day1-post.md to confirm they read/render cleanly; run `cargo test` (no Rust changes expected — confirms the tree is still green); commit both files (and the PNG if produced) to the current wt/* branch.

## Reviews
1 1fc1d92babc47b5db9e37607fd3affdfe5dc32a6
