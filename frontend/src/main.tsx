// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { installContextMenuGuard } from "./lib/contextMenu";
import "./styles/globals.css";

// Suppress the webview's native right-click menu across the Mnemo UI, except
// inside editable fields (input/textarea/contenteditable) where Copy/Cut/Paste
// stays useful. The Browser tab is a separate child WebView2, so its context
// menu is independent and intentionally unaffected. Installed once at app entry
// (not inside a component/effect) so it is not double-installed under React
// StrictMode and lives for the page lifetime.
installContextMenuGuard();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>
);
