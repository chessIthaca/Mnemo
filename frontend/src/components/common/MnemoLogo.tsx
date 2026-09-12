// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useId } from "react";

interface MnemoLogoProps {
  /** Tailwind size classes for the rendered <svg>; defaults to the sidebar-badge size. */
  className?: string;
}

/**
 * The Mnemo app logo as an inline-SVG React component — a 1:1 port of
 * `src-tauri/icons/icon-source.svg` (the icon source of truth adopted
 * 2026-08-27; if the icon changes there, mirror the paths/gradients here).
 * Renders the full badge: dark rounded square, the "M" graph mark with node
 * dots, and the blue active-memory node.
 *
 * The gradient/filter def ids are namespaced per instance (Sidebar About
 * button + About dialog header render simultaneously) — duplicate SVG ids
 * would make the second instance resolve its `url(#...)` refs against the
 * first instance's defs.
 */
export function MnemoLogo({ className = "h-10 w-10" }: MnemoLogoProps) {
  const uid = useId().replace(/:/g, "");
  const bgId = `${uid}-bg`;
  const glowId = `${uid}-glow`;
  const blurId = `${uid}-blur`;

  return (
    <svg viewBox="0 0 1024 1024" className={className} role="img" aria-label="Mnemo logo">
      <defs>
        <linearGradient id={bgId} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#0D1117" />
          <stop offset="1" stopColor="#171B23" />
        </linearGradient>
        <radialGradient id={glowId}>
          <stop offset="0" stopColor="#587CFF" stopOpacity=".55" />
          <stop offset="1" stopColor="#587CFF" stopOpacity="0" />
        </radialGradient>
        <filter id={blurId}>
          <feGaussianBlur stdDeviation="22" />
        </filter>
      </defs>

      {/* Badge background */}
      <rect width="1024" height="1024" rx="190" fill={`url(#${bgId})`} />
      <circle cx="730" cy="730" r="230" fill={`url(#${glowId})`} filter={`url(#${blurId})`} />

      {/* Mnemo graph/M mark */}
      <g
        fill="none"
        stroke="#E6E7EB"
        strokeWidth="42"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M210 700V300" />
        <path d="M210 300L512 540L814 300" />
        <path d="M814 300V700" />
      </g>

      {/* Nodes */}
      <g fill="#E6E7EB">
        <circle cx="210" cy="300" r="54" />
        <circle cx="512" cy="540" r="54" />
        <circle cx="814" cy="300" r="54" />
        <circle cx="210" cy="700" r="54" />
      </g>

      {/* Active memory node */}
      <circle cx="814" cy="700" r="125" fill={`url(#${glowId})`} />
      <circle cx="814" cy="700" r="57" fill="#587CFF" />
      <circle cx="814" cy="700" r="38" fill="#6D8BFF" />
    </svg>
  );
}