+++
title = "Suppress webview context menu in Mnemo UI (except editable fields + Browser tab)"
created = "2026-08-24"
+++

Suppress the webview's native right-click (context) menu across the Mnemo UI except inside editable fields (input/textarea/contenteditable), where Copy/Cut/Paste stays available. The Browser tab is a separate native child WebView2, so its context menu is independent and intentionally unaffected (no change to BrowserView.tsx).

Implementation: `frontend/src/lib/contextMenu.ts` — `installContextMenuGuard()` adds a capture-phase `contextmenu` listener on `document` that calls `preventDefault()` unless `isEditableTarget(e.target)` is true. `isEditableContext(tag, contentEditableAttr)` is the pure, DOM-free decision (case-insensitive `contenteditable` keyword matching per HTML spec: `""`/`"true"`/`"plaintext-only"` → editable; `"false"`/`"inherit"`/`null` → not; `INPUT`/`TEXTAREA` always editable). `isEditableTarget` resolves a Text node to its parentElement + walks `closest("[contenteditable]")`. Guard installed once at app entry in `main.tsx` (top-level, not an effect — no StrictMode double-install). 12 table-driven tests in `contextMenu.test.ts` (registered in vitest.config.ts). README feature bullet added.

Branch `feat/context-menu-guard` @ 00733eb (unmerged — on the feature branch; main reset back to e2b210f after an accidental direct-to-main commit was moved onto the feature branch). Review: .coding/reviews/2026-08-24-context-menu-guard-verify-review.md (PASS). Frontend-only change; tsc clean, 542 tests pass.
