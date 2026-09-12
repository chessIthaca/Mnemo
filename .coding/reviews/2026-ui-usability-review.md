# UI / Usability Review — myharness (2026 whole-codebase review)

**Reviewer perspective:** a real human's lived experience using the app — usability, readability, and the friction a first-time user hits. Reviewed by reading the React 18 + TypeScript + Tailwind frontend and reasoning about the actual rendered behavior, flows, and states (the GUI cannot be run in this environment).
**Scope:** `frontend/src/` — layout (`Sidebar`, `MainPanel`, `RightPanel`, `StatusBar`, `InputBar`, `ConfigDialog`, `SafetyToggleDialog`), chat (`Conversation`, `Message`, `ApprovalPrompt`, `DiffView`, `InflightBar`), views (`MdViewer`, `PlanProgress`, `DiffViewer`, `FileBrowser`, `ToolOutput`, `BacklogView`, `SafetyRules`, `StatsView`), state (`useAgentStore`, `useAgentEvents`), and styling (`globals.css`).
**Grounding:** `.coding/reviews/2026-review-baseline.md`, `PLAN.md` (UI layout + Build phases A–E), `frontend/src/styles/globals.css`.

---

## Executive summary

myharness's frontend is **usable-but-rough, leaning polished-in-places**. The core chat loop is genuinely well thought out: streaming is throttled by `requestAnimationFrame` batching (`useAgentEvents.ts:83-99`), `scrollIntoView` is throttled to 100ms (`Conversation.tsx:12-17`), the `Message` component is `memo`'d with a bespoke equality check (`Message.tsx:28-56,149`), and streaming text renders as plain text (markdown parsed once on finalize — `Message.tsx:11-17,84-94`). The approval flow is the strongest part — four escalating actions (Allow-for-project → Mark-Safe → Approve → Deny) with inline diff and clear color semantics (`ApprovalPrompt.tsx:108-174`). The inflight bar with token/context/rate telemetry is information-dense but legible (`InflightBar.tsx`).

The roughness is concentrated in three areas: **(1) accessibility is effectively absent** — a codebase-wide search for `aria-`, `role=`, `tabIndex`, `onKeyDown`, `prefers-reduced-motion`, and `prefers-color-scheme` returned **zero matches** across all 23 frontend source files; **(2) empty/discoverability states are thin** — a first-time user lands on a near-blank conversation with no onboarding, and the slash commands that power half the app are discoverable only by typing `/` then `/help`; **(3) light-theme polish is incomplete** — the dark theme is well-tuned, but the light theme tokens (`globals.css:36-44`) don't override the `.hljs-*` code colors or the scrollbar, so code blocks and scrollbars render dark-on-light. None of these block the product; together they define the gap between "works" and "modern and polished" (the PLAN.md Phase E bar).

---

## Overall usability verdict: **Usable-but-rough**

The interaction model is sound and the dark theme reads well. Accessibility and first-run experience are the clear weaknesses; light-theme and a few contrast/affordance details are the polish gaps.

---

## Persona walk-through (a first-time user)

**Open the app.** If the brain built, `App.tsx` mounts the layout: `Sidebar` (56px icon rail) + `MainPanel` (agent tabs + conversation) + `InflightBar` + `InputBar` + `StatusBar`, with `RightPanel` hidden by default (`App.tsx:193-204`). The conversation area shows the active agent's transcript — but on a fresh start **the transcript is empty and there is no welcome, hint, or example prompt**. The only text the user sees is the input placeholder: "Type your message... (Enter to send, Shift+Enter for newline, / for commands, paste images)" (`InputBar.tsx:326`). That placeholder is the *entire* onboarding. **Friction: a first-time user doesn't know what the app can do, that a plan is required, or that tools/right-panel exist.** Evidence: `frontend/src/components/chat/Conversation.tsx:57-72` (renders only `transcript` + `streamingText` + `pendingApproval`; no empty-state branch), `InputBar.tsx:323-327`.

**Type a prompt + Enter.** The user message appears immediately as a right-aligned cyan bubble (`Message.tsx:60-79`); `sendPrompt` fires (`InputBar.tsx:154`). The agent emits `Started`, the tab's dot pulses (`MainPanel.tsx:36-42` + `.agent-running-dot` animation `globals.css:168-180`), and the InflightBar appears with "thinking…" dots (`InflightBar.tsx:124-135`). Text streams in as plain text (good — no markdown jank mid-stream, `Message.tsx:84-94`). **This flow works well.** Auto-scroll is throttled so it stays pinned without jank (`Conversation.tsx:20-39`).

**A tool call fires.** A `ToolCard` appears (`Message.tsx:123,229-296`): collapsed by default, color-coded by state (yellow=running, slate=done, red=failed, `Message.tsx:247-261`), with a `✓`/`✗` and count. The header shows the tool name + a smart label (file basename, shell `purpose`, spawn_agent `name` — `Message.tsx:191-227`). **This is clear and well-designed.** Expanding shows pretty-printed args + result in scrollable `<pre>` (`Message.tsx:298-342`). One nit: the result `<pre>` is capped at `max-h-64` (`Message.tsx:336`) — long tool output is truncated to a scroll box, which is fine, but there's no "copy" or "open in Output tab" affordance from the card.

**A file_edit needs approval.** `ApprovalPrompt` renders inline (`Conversation.tsx:70-72`): a yellow-bordered card "Approve file_edit?" with the file path as a toggle to show the diff (`ApprovalPrompt.tsx:108-174`), and four buttons left-to-right: **Allow for project** (amber, only if project-scoped), **Mark Safe** (blue), **Approve** (green), **Deny** (red). After acting, it collapses to a one-line "file_edit approved/denied" (`ApprovalPrompt.tsx:93-106`). **This is the most polished part of the UI** — the escalation ladder is intuitive, the diff toggle is inline, and the resolved state is unobtrusive. Two friction points: (a) the diff is a *toggle* the user must click — there's no auto-open, and the file-path button doesn't visually read as "click to see what will change" (it's `text-slate-400 hover:text-slate-200`, `ApprovalPrompt.tsx:118-122`); (b) the buttons are ordered with the *most consequential escalation* ("Allow for project" — changes a global safety mode) first and the *safest per-action* ("Approve") third. A first-time user scanning left-to-right might click the leftmost button without reading.

**Check plan progress.** The user must (a) know the right panel exists, (b) click the "Plan" icon in the Sidebar (`Sidebar.tsx:21,52-70`) to enable that tab, (c) click the `PanelRight` toggle at the sidebar bottom to reveal the panel (`Sidebar.tsx:80-95`), then (d) click the "Plan" tab in the panel (`RightPanel.tsx:14,77-78`). That's a 4-step path to see the plan that the agent is required by the workflow to be following. **Friction: the plan — the central artifact of the plan-first workflow — is buried behind two toggles and a tab.** There's no auto-show of the plan panel when a plan is created/updated (the PRD's DiffViewer auto-shows on approval, but plan progress has no equivalent). Evidence: `Sidebar.tsx:36-43`, `RightPanel.tsx:24-66`, `App.tsx:202`.

**Run a slash command.** The user types `/` — but there's **no autocomplete, no command menu, no hint of available commands**. They must know to type `/help`, which injects the help text as an assistant message (`InputBar.tsx:179-184`, `slash.ts:41-48`). The help text itself says `/model` and `/provider` take effect only "on restart" (`slash.ts:43-44`) — a real limitation surfaced only here. `/save` with no path writes somewhere (the resolved path is echoed back, `InputBar.tsx:197-201`), and `/load` requires an exact path the user must somehow know (`InputBar.tsx:210-243`). **Friction: slash commands are powerful but undiscoverable and path-bearing ones have no file picker.**

**Switch a view.** The 8 right-panel views (md/plan/diff/output/files/safety/stats/backlog) are each an icon in the sidebar with on/off + a panel-reveal toggle (`Sidebar.tsx:50-70`). The "disabled" state is a line-through + opacity-50 (`Sidebar.tsx:62-64`) — **line-through on an icon button is an unusual affordance** that a first-time user may not read as "off, click to turn on." The `title` attribute clarifies (`Sidebar.tsx:65`) but tooltips require hover. **Friction: the per-view enable/disable + panel-toggle model is more complex than a simple tab bar; the mental model isn't taught anywhere.**

**Error / stuck states.** The startup-error screen is genuinely good — a centered AlertTriangle, the error in a scrollable `<pre>`, and a Reload button (`App.tsx:167-191`). Tool errors render as a red-bordered card ("error: …", `Message.tsx:125-131`). Provider errors with `retrying: true` keep the agent "running" (`channels.rs:138-147`) so the UI shows the thinking dots — **but I see no visible "retrying (attempt N/3)" rendering in the transcript**; the `Error { retrying }` event is handled in the store but the retry note doesn't appear to surface as a transcript entry a user can see (would need to confirm in `useAgentStore.handleAgentEvent`). An approval left unanswered blocks the agent silently — the approval card stays, but there's no timeout/abandon affordance. **Friction: retry progress and stuck-approval states are under-communicated.**

---

## Findings

### 1. Layout & visual hierarchy — ✅ mostly sound

The three-region split (Sidebar 56px / MainPanel flex-1 / RightPanel 40% min-300px, `App.tsx:193-204`, `RightPanel.tsx:37`) gives the conversation the most space, which is correct. The conversation is centered with `max-w-4xl` (`Conversation.tsx:56`) — good line length. The right panel at `w-2/5 min-w-[300px]` (`RightPanel.tsx:37`) is a fixed proportion, **not user-resizable** (only the InflightBar is drag-resizable, `InflightBar.tsx:64-98`). On a narrow window the min-300px right panel can squeeze the chat. **Medium — the panel split isn't draggable; small windows squeeze the chat.** Evidence: `RightPanel.tsx:37`, `App.tsx:196-202`.

### 2. Readability — ⚠️ dark good, light incomplete

Dark theme tokens are well-chosen: `--bg-primary #0f172a` / `--text-primary #e2e8f0` is high-contrast; muted `#94a3b8` for secondary text (`globals.css:11-16`). Prose is styled via `prose prose-invert prose-sm` (`Message.tsx:97`) with tables/lists/blockquotes overridden in `globals.css:182-194`. Code blocks get a header bar with language label + hover-reveal copy button (`Message.tsx:151-188`) — **genuinely nice**. Font family + size are user-configurable via CSS vars applied at mount (`App.tsx:38-66`).

**High — light theme is half-implemented.** The `.light` block (`globals.css:36-44`) overrides only bg/text/border/muted. It does **not** override the `.hljs-*` code colors (`globals.css:122-135` use `--code-*` vars that are *only* defined in `:root` dark, lines 26-32) — so in light theme, code blocks render with **dark-theme syntax colors on a light background** (the `pre.hljs` bg is `--bg-primary` which light sets to `#ffffff`, but token colors like `#ff7b72`/`#a5d6ff` are tuned for dark). The scrollbar track/thumb use `--bg-secondary`/`--bg-tertiary` (`globals.css:96-104`) which light overrides, so scrollbars are OK — but the thinking-dots and `.prose a` hardcode `#22d3ee` (`globals.css:147,193`) which is fine on both. Net: **code highlighting is broken in light theme.** Evidence: `globals.css:26-44,113-135`.

**Medium — inline code contrast.** Inline code is `text-pink-300` on `bg-bg-tertiary` (`Message.tsx:107`). On dark that's readable; pink-300 (#f9a8d4) on #334155 is ~3.5:1 — borderline for small text. Evidence: `Message.tsx:106-112`.

### 3. Interaction clarity & affordances — ⚠️ mixed

The approval flow is clear (§walk-through). Tool cards are clear. But several affordances are weak:
- **The file-path diff toggle doesn't read as a button** — it's plain text with a hover color change (`ApprovalPrompt.tsx:116-123`). A user may not realize clicking it shows the diff. **Low.**
- **Sidebar "disabled" = line-through + opacity** (`Sidebar.tsx:62-64`) — unconventional; relies on the `title` tooltip. **Low.**
- **The InflightBar's drag handle** is a 6px-tall strip (`InflightBar.tsx:107-113`) — discoverable only by cursor change to `ns-resize`. No label. **Low.**
- **Button order in approvals** puts the highest-escalation action first (§walk-through). **Medium** — consider ordering least→most consequential, or visually separating the escalations from Approve/Deny.

### 4. Discoverability — ⚠️ the app's biggest UX gap

- **No onboarding / empty state.** A fresh conversation is blank with only the input placeholder as guidance (`Conversation.tsx`, `InputBar.tsx:326`). No example prompts, no "the agent works plan-first," no pointer to the right panel. **High.**
- **Slash commands have no autocomplete/menu.** Typing `/` gives no feedback; the user must know `/help` exists (`slash.ts`, `InputBar.tsx:122-129`). **Medium.**
- **The plan is buried** behind enable-tab + reveal-panel + select-tab (§walk-through). The central plan-first artifact has no surfacing trigger. **High.**
- **`/model` and `/provider` require restart** — surfaced only in `/help` text (`slash.ts:43-44`), not at the moment of use. A user switching models gets no "restart needed" confirmation. **Medium.**
- **`/save` / `/load` need paths** with no file picker (`InputBar.tsx:191-243`). **Medium.**

### 5. Feedback & state visibility — ✅ mostly strong, ⚠️ two gaps

- **Streaming feedback** is good: pulsing tab dot + "thinking…" dots + token/context telemetry (`MainPanel.tsx:36-42`, `InflightBar.tsx:124-135,149-207`). The context bar color-codes by fill (green/amber/red, `InflightBar.tsx:58-60`). **Strong.**
- **Tool state** is clear (running/done/failed colors + ✓/✗, `Message.tsx:247-285`). **Strong.**
- **Startup-error screen** is helpful and actionable (`App.tsx:167-191`). **Strong.**
- **Gap — retry progress not surfaced.** Provider retries emit `Error { retrying: true }` (`channels.rs:144-147`), but I find no transcript rendering of "retrying (attempt N/3)" — the user sees the agent still "thinking" with no explanation of repeated failures. **Medium.** Evidence: needs confirmation in `useAgentStore.handleAgentError`; the `retrying` flag is plumbed but its UI rendering wasn't evident in the components read.
- **Gap — unanswered approval has no timeout/abandon.** The card waits indefinitely (`ApprovalPrompt.tsx`); no affordance to "deny all" is exposed in the card (DenyAll exists in the Rust `Approval` enum, `channels.rs:304-311`, but the UI only shows Approve/Deny, `ApprovalPrompt.tsx:144-157`). **Low-Medium.**

### 6. Accessibility — ❌ effectively absent (the highest-impact gap)

A codebase-wide search for `aria-`, `role=`, `tabIndex`, `onKeyDown` (outside the input textarea), `prefers-reduced-motion`, and `prefers-color-scheme` across all 23 frontend files returned **zero matches**. Concretely:

- **No ARIA roles/labels** on the tab bars (`MainPanel.tsx:20-50` agent tabs, `RightPanel.tsx:39-66` view tabs) — they're `<button>`s in a flex row, not `role="tab"`/`role="tablist"`, so a screen reader announces them as loose buttons. **High.**
- **No focus management** on dialogs. `ConfigDialog` and `SafetyToggleDialog` are conditionally rendered (`Sidebar.tsx:97`, `ApprovalPrompt`) with no focus trap, no `role="dialog"`, no `aria-modal`, no focus restoration on close. Tabbing can escape into the background UI. **High.**
- **Color-only status signals.** Agent running/idle is conveyed by a pulsing-vs-hollow dot (`MainPanel.tsx:36-42`); tool running/done/failed uses yellow/slate/red borders (`Message.tsx:247-261`). The tool card *does* add `✓`/`✗` text (`Message.tsx:282`) — good — but the agent-tab dot has no text alternative beyond the `title` attribute (`MainPanel.tsx:34`). For color-blind users the dot's animation helps, but the idle hollow-vs-filled distinction is color+shape only. **Medium.**
- **No `prefers-reduced-motion` handling.** The thinking-dots, agent-pulse, and count-up animations (`globals.css:137-180`, `useCountUp.ts`) always animate. Motion-sensitive users can't disable them. **Medium.**
- **No `prefers-color-scheme`** — theme is manual toggle only (`App.tsx:56`), ignoring the OS preference. **Low.**
- **Keyboard nav is partial.** The textarea handles Enter/Shift+Enter (`InputBar.tsx:250-255`) — good. But there are no keyboard shortcuts for approve/deny (a user must tab to the button), no shortcut to toggle the panel, and the slash-command `/panel` is the only keyboard path to the right panel. **Medium.**
- **Images have alt text** (`Message.tsx:72`, `InputBar.tsx:298`) — the one accessibility positive found. **Good.**

**This is the single highest-impact improvement area.** The app is a keyboard-heavy power-user tool, yet keyboard and screen-reader support are minimal.

### 7. Friction & edge cases — ⚠️ several

- **Empty conversation** → blank screen (§4). **High.**
- **No agents** → "No agents" / "No agent active" text (`MainPanel.tsx:47-49,56-58`) — present but minimal; no "how to start." **Low.**
- **Very long streamed responses** → throttled well (rAF + 100ms scroll), plain-text streaming, memoized messages. **Handled.** But the streaming `<div>` has no max-height; a giant response grows the scroll area (correct, but no "jump to bottom" button if the user scrolls up). **Low.**
- **Large file in FileBrowser/MdViewer** — `MdViewer` renders via ReactMarkdown; a huge file re-parses on every render. No virtualization evident. **Low-Medium.** Evidence: `views/MdViewer.tsx`.
- **No git repo** → `getGitBranch` failure is `console.error`'d (`App.tsx:103-108`); the status bar would show empty/no branch. No user-facing "not a git repo" state. **Low.**
- **Plan with 0 steps** — `PlanProgress` rendering not deeply inspected, but the workflow guarantees ≥1 step on create. **Low.**

### 8. Polish vs PRD intent — ⚠️ short of "modern and polished"

PLAN.md Phase E wants "modern, polished styling via Tailwind + shadcn/ui" + slash commands + theming + conversation save/load + live streaming + diffs (PLAN.md lines 428-441, 485-486). Status:
- ✅ Tailwind is used throughout; dark theme is polished.
- ❌ **shadcn/ui is not used.** `package.json` has no `@radix-ui/*` deps — the PLAN.md-specified component library (buttons, dialogs, tabs, scroll-area) was **not adopted**; the app uses hand-rolled Tailwind components instead (e.g. `ConfigDialog` is a custom modal, not a Radix Dialog). This is a **material deviation from the PRD's "modern-looking baseline"** and explains the accessibility gaps (Radix provides focus traps, ARIA, keyboard nav for free). **High (PRD + a11y).** Evidence: `frontend/package.json:11-32` (no `@radix-ui`), PLAN.md line 54.
- ⚠️ Slash commands work but lack autocomplete (§4). **Medium.**
- ✅ Theming (dark/light toggle + accent color + font) is implemented and persisted (`App.tsx:38-66`, `useAgentStore` apply* funcs) — but light theme is incomplete (§2). **Medium.**
- ✅ Conversation save/load implemented (`InputBar.tsx:191-243`) — but path-only, no picker. **Medium.**
- ✅ Live streaming + diffs implemented and throttled. **Strong.**

---

## What's done well

- **Streaming performance** — rAF batching, 100ms scroll throttle, memoized `Message`, plain-text-during-stream. The prior "H2 streaming jank" finding is fully addressed. Evidence: `useAgentEvents.ts:83-99`, `Conversation.tsx:12-39`, `Message.tsx:28-56,84-94`.
- **The approval escalation ladder** (Allow-for-project → Mark-Safe → Approve → Deny) is thoughtful and the inline diff toggle is well-placed. Evidence: `ApprovalPrompt.tsx:108-174`.
- **The InflightBar telemetry** — tokens, context bar (color-coded by fill), TTFT/generation tok/sec — is genuinely useful and well-formatted. Evidence: `InflightBar.tsx:149-207`.
- **Tool cards** — collapsed-by-default, state-colored, smart labels (file basename / shell purpose / agent name), ✓/✗ + count. Evidence: `Message.tsx:191-296`.
- **Code blocks** ��� language label + hover copy button, careful `.hljs` box-vs-inline scoping (`globals.css:107-135`, `Message.tsx:151-188`).
- **Startup-error screen** — actionable, not a crash. Evidence: `App.tsx:167-191`.

---

## Top 5 usability fixes (highest user impact)

1. **Add accessibility infrastructure** — ARIA roles on the two tab bars, `role="dialog"`+focus-trap+restore on `ConfigDialog`/`SafetyToggleDialog`, keyboard shortcuts for Approve/Deny (e.g. `A`/`D`), and a `prefers-reduced-motion` guard. The cheapest high-impact path is adopting **shadcn/ui (Radix)** as the PRD specified — it provides most of this for free. **High.** Evidence: zero matches codebase-wide; `package.json` has no Radix.
2. **Add a first-run empty state** — when the transcript is empty, show a centered welcome with 2-3 example prompts and a one-line explanation of the plan-first workflow + a pointer to the right panel. **High.** Evidence: `Conversation.tsx:57-72`.
3. **Surface the plan automatically** — auto-open the right panel on the Plan tab (or show a compact plan strip) when a plan is created or a step completes, mirroring the DiffViewer's approval-auto-show. The plan is the workflow's central artifact and is currently buried. **High.** Evidence: `Sidebar.tsx:36-43`, `RightPanel.tsx:24-66`.
4. **Fix light-theme code highlighting** — define `--code-*` vars in the `.light` block (or use a light hljs theme) so code blocks don't render dark-on-light. **Medium-High.** Evidence: `globals.css:26-44,122-135`.
5. **Add slash-command autocomplete** — on `/`, show a small menu of available commands with descriptions; surface "restart required" inline for `/model`/`/provider`; add a file picker for `/save`/`/load`. **Medium.** Evidence: `slash.ts`, `InputBar.tsx:122-129`.