+++
title = "render + verify a social card PNG (docs/marketing/card-to-png.html, dpr 1.5 math, pixel checks, claim sweep)"
created = "2027-01-11"
+++

Workflow for producing a 1200x630 upload-ready PNG from a card SVG. The files live OUTSIDE git: assets/social/ and docs/marketing/ are gitignored (see the DECISION record "marketing artifacts are not tracked in git"; removal plan 5df6b409). This machine has no Inkscape/ImageMagick/resvg — only Pillow and the app's Browser tab.

(1) Author the card SVG (viewBox 0 0 1200 630) — light editorial ground, a ~64px hero headline as the single focal point, one mechanism strip, two muted 17px claim lines; the repo's own light palette (cyan #0891b2/#0e7490, slate #64748b/#94a3b8) beats a dark-SaaS look.

(2) Render via docs/marketing/card-to-png.html in the app's Browser tab: file:///C:/Mnemo/docs/marketing/card-to-png.html?v=N (bump N to bust cache). The page shows the card at 800x420 CSS px because the Browser tab runs at devicePixelRatio 1.5 in a 910x1280 CSS viewport, so 800x420 CSS = exactly 1200x630 device px — 1200 CSS px would CLIP the right edge. The sandboxed offscreen browser cannot load file:// at all and is fixed at 800x600; use the Browser tab.

(3) Before screenshotting, browser_eval that img.complete is true and naturalWidth == 1200 — an early capture races the navigation and yields the previous frame.

(4) browser_screenshot, then Pillow-crop (0,0,1200,630). The harness's magenta background makes bleed detectable — assert 0 magenta pixels inside the crop.

(5) Trust pixel evidence over the vision/OCR tools, which returned blank or non-answers on some rounds: measure text-run extents with a column scan and centre arrows in the MEASURED gaps (rendered text runs ~30px wider than estimates; a 2px near-touch was only caught by measuring).

(6) Delete diagnostic screenshots under .coding/browser/screenshots/ before committing anything.

SECURITY CAVEAT: the harness's ?src= parameter (reading location.search and assigning to img.src) is what CodeQL flagged as client-side XSS + client-side URL redirect at card-to-png.html:35 (alerts security/code-scanning/16 and /17). Drop that feature before this file ever goes back into git.
