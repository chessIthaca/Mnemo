// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * App-shell streaming re-render contract (mem-perf review HIGH 1).
 *
 * App used to subscribe to the whole active-agent object
 * (`s.agents[s.activeAgent]`) whose identity changes on every rAF
 * streaming flush (appendStreamingText spreads the agent per flush) —
 * so the entire app tree (Sidebar, MainPanel, InputBar, StatusBar,
 * RightPanel — none memoized) reconciled per streaming frame: the
 * dominant streaming-jank source on long transcripts, competing with
 * typing in InputBar during streaming.
 *
 * The contract, pinned as source contracts (the components' runtime
 * wiring is store-driven, so static source is the stable surface — same
 * style as InflightBar.test.ts):
 *   1. App must NOT select `s.agents[s.activeAgent]` — the subscription
 *      moved down into InflightBar (keyed by the stable agentId), so a
 *      streaming flush re-renders only the bar.
 *   2. InflightBar subscribes itself (`s.agents[agentId]`) and gates on
 *      the agent existing (renders nothing otherwise).
 *   3. The prop-less shell components (Sidebar/InputBar/StatusBar) are
 *      memo()d, so future App-level re-renders can't drag them along.
 *
 * Sources are imported via Vite's `?raw` (typed by `vite/client`, so
 * this compiles under `tsc` too).
 */
import { describe, expect, it } from "vitest";
import appSource from "./App.tsx?raw";
import inflightSource from "./components/chat/InflightBar.tsx?raw";
import sidebarSource from "./components/layout/Sidebar.tsx?raw";
import inputBarSource from "./components/layout/InputBar.tsx?raw";
import statusBarSource from "./components/layout/StatusBar.tsx?raw";

describe("App shell streaming re-render (mem-perf HIGH 1)", () => {
  it("App no longer selects the whole active-agent object", () => {
    // The agent object's identity changes on every rAF streaming flush;
    // subscribing to it re-rendered the entire app shell per frame. The
    // subscription must live in InflightBar (below), keyed by agentId.
    expect(appSource).not.toContain("s.agents[s.activeAgent]");
  });

  it("App mounts InflightBar by its stable agentId only", () => {
    expect(appSource).toContain("<InflightBar agentId={activeAgent} />");
  });

  it("InflightBar subscribes itself, keyed by its agentId", () => {
    expect(inflightSource).toContain("s.agents[agentId]");
  });

  it("InflightBar renders nothing while the agent doesn't exist", () => {
    expect(inflightSource).toContain("if (!state) return null;");
  });

  it("the prop-less shell components are memoized", () => {
    // Complementary hardening: these three take no props and
    // self-subscribe, so memo() fully shields them from App-level
    // re-renders (MainPanel is deliberately NOT memoized — it must
    // re-render during streaming).
    expect(sidebarSource).toContain("= memo(function Sidebar()");
    expect(inputBarSource).toContain("= memo(function InputBar()");
    expect(statusBarSource).toContain("= memo(function StatusBar()");
  });
});
