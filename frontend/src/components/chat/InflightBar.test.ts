// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * InflightBar context-popup hover-bridge contract test.
 *
 * The ctx bar's hover popup (breakdown + "Compact context" button) is shown
 * via `group-hover`. The gap between the bar and the popup MUST be bridged
 * with padding (`pb-1`) on the positioned wrapper — padding stays inside the
 * element's hover box. A margin (`mb-1`) is a dead zone: the mouse crossing
 * it breaks `group-hover` and the popup hides before the Compact button can
 * be reached (user-reported bug: the button could never be pressed).
 *
 * This project has no React DOM test infra (vitest runs in `node`
 * environment), so this is a static source-contract test in the style of
 * `src/lib/ipc-contract.test.ts`: it reads the component source (via Vite's
 * `?raw` import — typed by `vite/client`, so it compiles under `tsc` too)
 * and asserts the bridge classes are present.
 */

import { describe, expect, it } from "vitest";
import source from "./InflightBar.tsx?raw";

describe("InflightBar context popup hover bridge", () => {
  it("bridges the bar→popup gap with pb-1 padding on the positioned wrapper", () => {
    // The wrapper carries both the positioning (bottom-full) and the hover
    // toggle (group-hover:block) plus the pb-1 bridge padding.
    expect(source).toContain("pb-1 group-hover:block");
  });

  it("does NOT use a margin on the popup wrapper (the old dead-zone bug)", () => {
    // A margin anywhere on the positioned wrapper (the className containing
    // bottom-full) re-introduces the dead zone. Other elements in the file
    // legitimately use mb-1 — the check is scoped to the wrapper's className.
    expect(source).not.toMatch(/bottom-full[^"]*\bmb-\d/);
  });

  it("keeps the Compact button wired to the compact IPC call", () => {
    expect(source).toContain("compact(agentId)");
  });
});

describe("InflightBar empty-reasoning state", () => {
  it("always renders the expand/collapse chevron (never gated on activity)", () => {
    // Regression (user-reported): the chevron was gated on
    // `(expanded || hasActivity)`, so when the reasoning text was still empty
    // (activityLog had no entries) the triangle vanished and the panel could
    // not be opened at all.
    expect(source).not.toContain("(expanded || hasActivity) &&");
  });

  it("bar click always toggles the panel (never gated on activity)", () => {
    // Same bug: the click handler ignored clicks while the log was empty.
    expect(source).toContain("onClick={() => setExpanded(!expanded)}");
    expect(source).not.toContain("if (expanded || hasActivity) setExpanded(");
  });

  it("exposes the toggle state to assistive tech", () => {
    expect(source).toContain('aria-expanded={expanded}');
    expect(source).toContain('aria-label="Toggle reasoning panel"');
  });

  it("renders the open-but-empty panel as an empty gray space (no placeholder glyph)", () => {
    // Regression (user-reported): the expanded empty panel rendered a lone
    // "—" that read as a rendering artifact when resizing; it should just be
    // an empty gray space. The `>—<` JSX-text form only existed in that
    // placeholder (other "—" uses are string literals inside expressions).
    expect(source).not.toContain(">—<");
    // Entries render only when there ARE entries.
    expect(source).toContain("hasActivity &&");
  });
});

describe("InflightBar drag-handle visibility", () => {
  it("grip pill is bright enough to read as a handle (slate-500, not bg-border)", () => {
    // Regression (user-reported "too dark horizontal resize bar"): the pill
    // was bg-border (#334155) on bg-bg-secondary (#1e293b) — same color
    // family, so the handle vanished against the bar. The pill must use a
    // brighter token (slate-500) at rest and brighten further on hover.
    expect(source).toContain("bg-slate-500");
    expect(source).toContain("group-hover:bg-slate-400");
  });

  it("drag-handle host carries the `group` class so the hover state reaches the pill", () => {
    // `group-hover:bg-slate-400` only fires if an ancestor carries `group`.
    // The handle host must stay `group` (dropping it would silently freeze
    // the pill at its rest color).
    expect(source).toMatch(/className="group flex h-1\.5 shrink-0 cursor-ns-resize/);
  });
});

describe("InflightBar live phase indicator", () => {
  it("cycles the status label through the request-loop phases", () => {
    // The static "thinking…" label was replaced by a phase-aware map:
    // sending / waiting / reasoning / answering (streaming) / running tools.
    expect(source).toContain('return "sending…"');
    expect(source).toContain('return "waiting…"');
    expect(source).toContain('return "reasoning…"');
    expect(source).toContain('return "answering…"');
    expect(source).toContain('return "running tools…"');
  });

  it("overlays approval + question pauses with amber labels", () => {
    expect(source).toContain('return "waiting for approval…"');
    expect(source).toContain('return "waiting for your answer…"');
    expect(source).toContain("text-amber-400");
  });

  it("renders the live elapsed timer from turnStartedAt", () => {
    // The timer ticks every second while running and formats "1m 3s" /
    // "22s" via fmtDuration.
    expect(source).toContain("setInterval(() => setNow(Date.now()), 1000)");
    expect(source).toContain("fmtDuration(elapsedMs)");
    expect(source).toContain("turnStartedAt");
  });

  /**
   * Regression (user report 2027-01-07): the counters animated toward
   * their targets via the useCountUp rAF interpolation, which was
   * distracting — the underlying values are already event-driven (live
   * chars/4 estimates per stream chunk, authoritative counts on the
   * Usage event), so the display must render the raw value directly and
   * change exactly when data arrives, like the trace graph. The counter
   * values = accumulated usage + the bucket's live chars/4 estimate, so
   * the numbers advance while deltas stream in — reasoning deltas lift
   * the 🧠 counter (not just ↓) during the thinking phase. A named
   * function (not an inline arrow) so the code graph indexes it as a
   * symbol and the bug plan's regression-test gate can point at the
   * actual test function.
   */
  function countersRenderPerEventNoInterpolation(): void {
    expect(source).toContain("const completionDisplay = tokenUsage.completion + liveCompletionTokens;");
    expect(source).toContain("const reasoningDisplay = tokenUsage.reasoning + liveReasoningTokens;");
    // The interpolation hook must be gone entirely (deleted from
    // src/hooks — no callers remain).
    expect(source).not.toContain("useCountUp");
  }

  it("updates the ↓/🧠 token counters per event via the live estimates — no count-up interpolation", countersRenderPerEventNoInterpolation);

  it("shows the 🧠 counter during the first reasoning phase — the gate admits the live estimate, not just landed usage", () => {
    // Regression (user report 2027-01-04): the counter's target rides
    // liveReasoningTokens (chars/4 per reasoning delta), but the render gate
    // used to require tokenUsage.reasoning > 0 — a value only set by the
    // Usage event AFTER a response completes. During the first (often
    // longest) reasoning phase of a session the counter was hidden and the
    // stats row sat frozen while the reasoning panel streamed; everything
    // refreshed at once when usage landed. The gate must also admit the live
    // estimate so the counter ticks continuously throughout the phase.
    expect(source).toContain("tokenUsage.reasoning > 0 || liveReasoningTokens > 0");
    expect(source).not.toContain("{tokenUsage.reasoning > 0 && (");
  });
});
