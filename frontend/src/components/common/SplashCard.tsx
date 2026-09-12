// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useId, type ReactNode } from "react";
import { MnemoLogo } from "./MnemoLogo";

interface SplashCardProps {
  /**
   * The dialog content, rendered in a `p-5` slot BELOW the splash band
   * (title row, progress block, buttons — whatever the dialog needs).
   */
  children: ReactNode;
  /**
   * Extra Tailwind classes for the card root (callers keep their width,
   * e.g. `w-96`); merged with the base card styling.
   */
  className?: string;
}

/**
 * A branded dialog card with a Mnemo splash band on top — the deep-navy
 * "network" look of the promo artwork, recreated entirely with inline SVG +
 * the existing MnemoLogo (no raster assets, matching the frontend's
 * no-binary-assets pattern). Used by the full-screen startup dialogs
 * (IndexingOverlay, the App.tsx reconcile dialog) so a wait looks like the
 * product, not like an error.
 *
 * The band: diagonal navy gradient (`#050A16` → `#0D1B33`), a soft network
 * motif of blue nodes/edges behind a centered brand stack (logo badge,
 * "mnemo" wordmark, "SMART CODING HARNESS", "MEMORY. SPEC. IMPROVEMENT.").
 * All brand text is JSX (not baked into the SVG) so it stays crisp at any
 * size and is testable via renderToStaticMarkup.
 *
 * Like MnemoLogo, the SVG def ids are namespaced per instance via useId —
 * the IndexingOverlay and the reconcile dialog can render simultaneously
 * (both mount in App's tree) and duplicate gradient ids would make the second
 * card resolve `url(#...)` refs against the first card's defs.
 */
export function SplashCard({ children, className = "w-96" }: SplashCardProps) {
  const uid = useId().replace(/:/g, "");
  const glowId = `${uid}-splashglow`;

  return (
    <div
      className={`overflow-hidden rounded-xl border border-border bg-bg-tertiary shadow-2xl ${className}`}
    >
      {/* Splash band — deep navy + network motif behind the centered brand. */}
      <div className="relative h-36 overflow-hidden border-b border-cyan-500/20">
        <div
          className="absolute inset-0"
          style={{
            background: "linear-gradient(135deg, #050A16 0%, #0D1B33 100%)",
          }}
        />
        <svg
          viewBox="0 0 384 144"
          preserveAspectRatio="xMidYMid slice"
          className="pointer-events-none absolute inset-0 h-full w-full"
          aria-hidden="true"
        >
          <defs>
            <radialGradient id={glowId}>
              <stop offset="0" stopColor="#587CFF" stopOpacity=".35" />
              <stop offset="1" stopColor="#587CFF" stopOpacity="0" />
            </radialGradient>
          </defs>
          {/* Soft glow behind the centre of the brand stack. */}
          <circle cx="192" cy="64" r="70" fill={`url(#${glowId})`} />
          {/* Network edges (a dashed pair, like the promo art). */}
          <g stroke="#2E4A8F" strokeWidth="1">
            <path d="M28 30 L96 58 L60 116" />
            <path d="M96 58 L192 36" strokeDasharray="4 4" />
            <path d="M192 36 L288 52" />
            <path d="M288 52 L352 24" strokeDasharray="4 4" />
            <path d="M288 52 L318 112" />
            <path d="M60 116 L150 108" />
            <path d="M150 108 L250 118" strokeDasharray="4 4" />
            <path d="M250 118 L318 112" />
            <path d="M24 78 L96 58" />
          </g>
          {/* Network nodes — glowing blue dots of varying size. */}
          <g fill="#4C7DFF">
            <circle cx="28" cy="30" r="2.5" />
            <circle cx="96" cy="58" r="3.5" />
            <circle cx="60" cy="116" r="2" />
            <circle cx="192" cy="36" r="2" />
            <circle cx="288" cy="52" r="3" />
            <circle cx="352" cy="24" r="2.5" />
            <circle cx="150" cy="108" r="2" />
            <circle cx="250" cy="118" r="2.5" />
            <circle cx="24" cy="78" r="2" />
          </g>
          {/* The "active" node glows, matching the badge's blue node. */}
          <circle cx="318" cy="112" r="9" fill="#587CFF" fillOpacity=".35" />
          <circle cx="318" cy="112" r="3.5" fill="#6D8BFF" />
        </svg>
        {/* Brand stack — logo, wordmark, tagline, values. */}
        <div className="relative flex h-full flex-col items-center justify-center gap-1">
          <MnemoLogo className="h-12 w-12" />
          <div className="text-xl font-bold leading-tight tracking-tight text-slate-100">
            mnemo
          </div>
          <div className="text-[9px] font-semibold tracking-[0.3em] text-[#6D8BFF]">
            SMART CODING HARNESS
          </div>
          <div className="text-[8px] tracking-[0.25em] text-slate-400">
            MEMORY. SPEC. IMPROVEMENT.
          </div>
        </div>
      </div>
      {/* Dialog content slot. */}
      <div className="p-5">{children}</div>
    </div>
  );
}

interface SplashProgressProps {
  /** The left-hand status label ("Indexing files…", "Reconciling sources…"). */
  label: string;
  /** The right-hand mono counter ("12/240 files indexed", "…"). */
  right: string;
  /**
   * Fill percentage (0–100), or `null` when the total is unknown — the bar
   * then renders the same indeterminate 30% stub the dialogs used before.
   */
  pct: number | null;
}

/**
 * The progress block rendered inside a SplashCard by the startup dialogs:
 * a label/counter row above the slim cyan progress bar. Shared so the
 * IndexingOverlay and the App.tsx reconcile dialog stay pixel-identical.
 */
export function SplashProgress({ label, right, pct }: SplashProgressProps) {
  return (
    <>
      <div className="mb-1 flex justify-between text-[0.7em] text-slate-500">
        <span>{label}</span>
        <span className="font-mono">{right}</span>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-bg-secondary">
        <div
          className="h-full rounded-full bg-cyan-500 transition-all"
          style={{ width: `${pct === null ? 30 : pct}%` }}
        />
      </div>
    </>
  );
}
