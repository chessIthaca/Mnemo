// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

/// Catches render errors so a single component crash shows a message
/// instead of blanking the whole window.
export class ErrorBoundary extends Component<Props, State> {
  state: State = { hasError: false, error: null };

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: unknown) {
    console.error("UI render error:", error, info);
  }

  render() {
    if (this.state.hasError) {
      return (
        <div className="flex h-screen w-screen flex-col items-center justify-center gap-3 bg-slate-950 p-8 text-center">
          <h1 className="text-lg font-semibold text-red-400">
            Something went wrong
          </h1>
          <p className="max-w-xl text-sm text-slate-400">
            The UI hit a render error. Check the devtools console for details.
            Reloading the window usually clears it.
          </p>
          <pre className="max-w-xl overflow-auto rounded-lg border border-slate-700 bg-slate-900 p-3 text-left text-xs text-slate-300">
            {this.state.error?.message ?? "Unknown error"}
          </pre>
          <button
            onClick={() => window.location.reload()}
            className="mt-2 rounded-lg bg-cyan-600 px-4 py-2 text-sm font-medium text-white hover:bg-cyan-500"
          >
            Reload
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
