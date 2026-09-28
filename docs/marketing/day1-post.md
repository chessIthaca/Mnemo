# Day 1 — launch post

Post text and the shareable card for the Day-1 announcement.
Post: [`assets/social/day1-card.png`](../../assets/social/day1-card.png) (1200×630) · source: [`assets/social/day1-card.svg`](../../assets/social/day1-card.svg).

---

## The post (long-form — LinkedIn, Reddit, blog, dev.to)

> My coding agent forgot everything overnight. I paid flagship prices for it to re-learn my own repo. Every. Single. Day.
>
> Session ends, context resets. Tomorrow it re-reads the same 40 files, re-derives the plan it already had, decides differently, and pushes a confident wrong turn I spend an hour unwinding. That's not a model problem — it's an **allocation** problem.
>
> So I built **Mnemo**: an open-source (MIT) Rust + Tauri harness whose whole job is allocation — the right model, the right context, the right permissions, at each moment.
>
> 🧠 **Sharp model plans, cheap model executes, a third one reviews.** Per-state model slots with their own context and reasoning budgets. The plan is a file on disk, so the executor doesn't need to be brilliant — it needs to follow a recipe.
>
> ✂️ **Seven token-saving levers, on by default.** Delta re-reads (a file you've already read comes back as a skeleton or a diff, not the whole thing), command-output compression, archive-and-expand for oversized results, compaction survival, an S–F context-quality score, cache-safe lean-output nudges, and recall delta for repeat memories. Every lever logs before/after tokens to a live savings dashboard.
>
> 💾 **Memory that survives the session.** Four tiers — working → episodic → semantic → procedural — ranked by strength. Bug root causes, decisions, conventions: learned once, recalled automatically.
>
> 🗺️ **A knowledge graph, not grep.** Tree-sitter parses 12 languages into a per-project graph, so "who calls this, what breaks if I change it?" is one lookup.
>
> 🔒 **And it's safe to leave running — which is what makes the cheap model viable.** Mutation tools don't exist in the tool list until a plan is on disk. `git merge` and `git push` are gated *always*. The output compressor redacts credentials before the model sees large command output.
>
> Open source, Windows + macOS.
> 👉 https://github.com/chessIthaca/Mnemo
>
> I'm shipping this in the open. **Follow along — one lever a week**, with the measured savings, not the marketing.

---

## Short variant (X / Bluesky / Mastodon — 262 chars; 249 by X's URL weighting, where a URL counts as 23)

```
Your coding agent forgets overnight — and you pay flagship prices for it to re-learn.

Mnemo: sharp model plans → cheap model executes → a read-only reviewer audits. Persistent memory, code graph, 7 token levers.

MIT, Rust → https://github.com/chessIthaca/Mnemo
```

---

## Hashtags

`#AIcoding #Rust #OpenSource #DevTools #LLM #Tauri`

X/Bluesky: use the short variant and **at most two** of these — more reads as spam.
LinkedIn: put three at the very bottom, after the CTA.

---

## Image

**File to upload:** `assets/social/day1-card.png` — 1200×630, committed and ready to post (most platforms refuse SVG uploads).

**Source:** `assets/social/day1-card.svg`. Edit the SVG, then re-render the PNG:

| Route | Command / step |
|---|---|
| In-repo harness (no extra tooling) | open `docs/marketing/card-to-png.html`, screenshot the card's bounds → `day1-card.png` |
| Inkscape | `inkscape day1-card.svg -w 1200 -h 630 -o day1-card.png` |
| Node | `npx svgexport day1-card.svg day1-card.png 1200:630` |

The harness is a 20-line page: it shows the card at 800×420 CSS px, which rasterises to exactly 1200×630 device pixels on a 1.5× display scale. Its magenta background exists so the crop bounds are never guesswork — magenta inside the crop means the crop is wrong. Reuse it for the rest of the series (any 1200×630 card).

**Alt text** (paste into the platform's image-description field):

> Mnemo launch card. Headline: "Stop paying flagship prices for an agent that forgets." Below it, three boxes with arrows — a sharp model plans, a cheap model executes, a read-only reviewer audits. Four claim pills follow: four-tier memory that survives sessions; a tree-sitter code graph over 12 languages; seven token levers with a live savings dashboard; no edits without a plan, and git push always gated. Footer: github.com/chessIthaca/Mnemo — Rust + Tauri, Windows + macOS, MIT.

**Do not** put the numbers on the card yet — Day 1 sells the pain and the fix. Numbers land from post #2 on, straight off the Dashboard.

---

## The series this post promises

Keep the CTA honest — each post carries one number and one screenshot:

| # | Post | Number to pull |
|---|---|---|
| 1 | Delta reads — never re-send a file you already read | Savings dashboard, `delta_reads` row |
| 2 | The reviewer that can't say yes to itself | A quoted finding from `.coding/reviews/` |
| 3 | Approval that builds trust as a file (`Mark Safe` → `.coding/safety.toml`) | Screenshot/GIF of the prompt |
| 4 | Cheap model, sharp plan — same task, two runs | Cost `$A → $B` from the two runs |
| 5 | Memory that survives — day 1 vs day 12 bug hunt | The auto-recalled `BUG:` memory |
