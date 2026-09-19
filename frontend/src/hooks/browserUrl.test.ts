// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT.

/**
 * Unit tests for the `browser://url-changed` handler (frontend/src/hooks/
 * browserUrl.ts) — the frontend half of the child webview's page-load sync
 * (backlog 3f838ea1). The backend emits `browser://url-changed` on every
 * child-webview page load (agent-steered CDP navigations and in-child link
 * clicks alike); the handler must record the URL in the store's `browserUrl`
 * so BrowserView's URL box tracks the current URL.
 */

import { beforeEach, describe, expect, it } from "vitest";

import { useAgentStore } from "./useAgentStore";
import { handleBrowserUrlChanged } from "./browserUrl";

describe("handleBrowserUrlChanged", () => {
  beforeEach(() => {
    useAgentStore.setState({ browserUrl: "" });
  });

  it("records the child webview's current URL in the store", () => {
    handleBrowserUrlChanged("https://example.com/agent-steered");
    expect(useAgentStore.getState().browserUrl).toBe(
      "https://example.com/agent-steered",
    );
  });

  it("overwrites a stale URL (the box tracks the CURRENT page)", () => {
    useAgentStore.setState({ browserUrl: "https://old.example.com" });
    handleBrowserUrlChanged("https://new.example.com");
    expect(useAgentStore.getState().browserUrl).toBe("https://new.example.com");
  });
});
