// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT.

import { useAgentStore } from "./useAgentStore";

/**
 * Handler for the backend's `browser://url-changed` event — emitted on every
 * child-webview page load (the `child_webview_builder` page-load hook,
 * backlog 3f838ea1). Records the current URL in the store's `browserUrl` so
 * BrowserView's URL box tracks agent-steered CDP navigations and in-child
 * link clicks alike (the frontend's own `loadIntoChild` already syncs its
 * navigations; the agent's CDP path was invisible to it).
 *
 * Extracted as a pure (React-free) function so the node-env unit tests can
 * exercise the store wiring directly (the Tauri `listen()` subscription
 * itself stays in useAgentEvents' idempotent module-scope listener,
 * mirroring the browser-reveal listener).
 */
export function handleBrowserUrlChanged(url: string): void {
  useAgentStore.getState().setBrowserUrl(url);
}
