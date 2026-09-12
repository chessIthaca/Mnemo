# Plan: Config dialog in the left toolbar — font family + font size for the agent window, entry box, and reasoning preview

## Goal
Add a **Settings** button to the left `Sidebar` toolbar that opens a config dialog.
One of the dialog's items lets the user choose the **font family** and **font size (height)**
used in three places:
1. The **agent window** (the `Conversation` transcript — assistant/user/tool messages).
2. The **entry box** (the `InputBar` textarea).
3. The **reasoning preview** (the `InflightBar` activity panel).

The choice persists to `localStorage` and is applied app-wide via a CSS custom property
(`--app-font-family`, `--app-font-size`) consumed by the three components.

## Why
- The user wants to control the font used across the main reading/typing surfaces.
- This is a pure frontend concern — no Rust/IPC changes needed. The font choice is a
  display preference, stored locally, applied via CSS variables.

## Scope (frontend only)
- `frontend/src/hooks/useAgentStore.ts` — add `fontFamily`, `fontSize`, and setters.
- `frontend/src/styles/globals.css` — default CSS variables + a `.app-font` helper.
- `frontend/src/components/layout/Sidebar.tsx` — add a Settings (gear) button.
- `frontend/src/components/layout/ConfigDialog.tsx` — **new** modal dialog.
- `frontend/src/components/chat/Conversation.tsx` — apply font vars.
- `frontend/src/components/layout/InputBar.tsx` — apply font vars to the textarea.
- `frontend/src/components/chat/InflightBar.tsx` — apply font vars to the activity panel.

---

## Step 1 — Store: font family + font size state with localStorage persistence

### `frontend/src/hooks/useAgentStore.ts`
- Add to `AppState`:
  - `fontFamily: string` (default `"system-ui"`)
  - `fontSize: number` (default `14`, in px)
  - `setFontFamily: (f: string) => void`
  - `setFontSize: (n: number) => void`
- Initialize from `localStorage` keys `mh.fontFamily` / `mh.fontSize` (guard for SSR/no-window).
- In the setters, also write to `localStorage` so the choice survives reloads.
- Add a small `applyFontVars()` helper (exported) that writes the two CSS custom
  properties onto `document.documentElement.style` — called from the setters and on
  app mount.

## Step 2 — CSS variables + defaults

### `frontend/src/styles/globals.css`
- In `:root`, add:
  ```css
  --app-font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --app-font-size: 14px;
  ```
- Add a utility class `.app-font` that sets `font-family: var(--app-font-family)`
  and `font-size: var(--app-font-size)`.
- Update `body` to use `var(--app-font-family)` (keep the existing fallback list).

## Step 3 — ConfigDialog component (new)

### `frontend/src/components/layout/ConfigDialog.tsx`
- Props: `open: boolean`, `onClose: () => void`.
- Modal overlay (same pattern as `SafetyToggleDialog`): fixed inset, backdrop click closes.
- Header: gear icon + "Settings", close (X) button.
- Body sections:
  - **Appearance** section:
    - **Font family** — a `<select>` with a curated list:
      `system-ui`, `Inter`, `Consolas`, `JetBrains Mono`, `Fira Code`, `Monaco`,
      `Cascadia Code`, `Georgia`, `Times New Roman`.
      (These are common Windows/Mac fonts; unknown ones fall back gracefully.)
    - **Font size** — a number input (range 10–24) with a live preview line showing
      "The quick brown fox" rendered in the chosen font/size.
  - A live preview block that uses the current `fontFamily`/`fontSize` so the user
    sees the effect immediately.
- Footer: a "Done" button (closes). Changes apply live (no separate save needed).
- Reads/writes via `useAgentStore` selectors + setters.

## Step 4 — Sidebar: add the Settings button

### `frontend/src/components/layout/Sidebar.tsx`
- Add local `useState` for `configOpen`.
- Add a gear (`Settings` icon from lucide-react) button in the bottom cluster
  (above the right-panel toggle, in the `mt-auto` group).
- Render `<ConfigDialog open={configOpen} onClose={() => setConfigOpen(false)} />`.

## Step 5 — Apply font vars to the three target components

### `frontend/src/components/chat/Conversation.tsx`
- On the outer scroll container, add `style={{ fontFamily: "var(--app-font-family)", fontSize: "var(--app-font-size)" }}`
  so the transcript (assistant markdown, user bubbles, tool cards) inherits the font.

### `frontend/src/components/layout/InputBar.tsx`
- On the `<textarea>`, add `style={{ fontFamily: "var(--app-font-family)", fontSize: "var(--app-font-size)" }}`.

### `frontend/src/components/chat/InflightBar.tsx`
- On the activity panel scroll div (the `font-mono` container), replace the hardcoded
  `font-mono` with `style={{ fontFamily: "var(--app-font-family)", fontSize: "var(--app-font-size)" }}`.
  (Keep the `text-xs`/`leading-relaxed` classes; the size var overrides the base.)

## Step 6 — App mount: apply persisted font vars

### `frontend/src/App.tsx`
- On mount (in the existing `useEffect`), call `applyFontVars()` so the persisted
  choice is applied before first paint.

---

## Exit criteria
- A gear button in the left toolbar opens a Settings dialog.
- The dialog has a Font family dropdown and a Font size number input with live preview.
- Changing the font updates the agent window (Conversation), the entry box (InputBar
  textarea), and the reasoning preview (InflightBar activity panel) immediately.
- The choice persists across reloads via `localStorage`.
- `tsc --noEmit` is clean (run via `npm run build`'s tsc step, or `npx tsc --noEmit`).
