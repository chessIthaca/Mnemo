// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Shared number formatting for token displays and percentages. Every token
 * count or tok/s rate shown outside the Trace view (which keeps raw
 * comma-grouped integers) uses the same hybrid rule:
 *
 * - under 10,000: raw, comma-grouped, at most 1 decimal ("1,234", "9,999.5")
 * - 10,000 to 999,999: K suffix, at most 1 decimal ("12.3K")
 * - 1,000,000 and up: M suffix, at most 1 decimal ("1.2M", "1,234.6M")
 *
 * Percentages (0-100 scale, rendered as `${fmtPct(x)}%`) always show at
 * most one decimal digit: "99.8%", "55%", "100%" (backlog 9042b47c).
 */

/** Round to at most 1 decimal place (e.g. 12.34 → 12.3). */
function round1(v: number): number {
  return Math.round(v * 10) / 10;
}

/**
 * The internal hybrid formatter: sign preserved, magnitude reduced to
 * thousands/millions with a K/M suffix, everything else comma-grouped with
 * at most 1 decimal. The threshold is chosen on the ROUNDED value so a
 * value that rounds across a boundary (e.g. 9,999.96 → "10K", 999,950 →
 * "1M") still gets the suffix its displayed magnitude warrants.
 */
function fmtHybrid(n: number): string {
  const abs = Math.abs(n);
  const r = round1(abs);
  if (r === 0) return "0"; // never render "-0"
  const sign = n < 0 ? "-" : "";
  if (r >= 1_000_000) {
    const mantissa = round1(r / 1_000_000).toLocaleString("en-US", {
      maximumFractionDigits: 1,
    });
    return `${sign}${mantissa}M`;
  }
  if (r >= 10_000) {
    const mantissa = round1(r / 1_000);
    if (mantissa >= 1000) {
      // The rounded K mantissa spilled to 1,000 — roll over to M.
      const m = round1(r / 1_000_000).toLocaleString("en-US", {
        maximumFractionDigits: 1,
      });
      return `${sign}${m}M`;
    }
    return `${sign}${mantissa.toLocaleString("en-US", {
      maximumFractionDigits: 1,
    })}K`;
  }
  return `${sign}${r.toLocaleString("en-US", {
    maximumFractionDigits: 1,
  })}`;
}

/**
 * Format a token count using the hybrid rule (see module doc): "1,234",
 * "12.3K", "1.2M".
 */
export function fmtTokens(n: number): string {
  return fmtHybrid(n);
}

/**
 * Format a tok/s rate using the same hybrid rule (see module doc): "42.5",
 * "1,234.5", "12.3K".
 */
export function fmtRate(n: number): string {
  return fmtHybrid(n);
}

/**
 * Format a percentage on the 0-100 scale for display: at most ONE decimal
 * digit, integers kept integral — 99.3 → "99.3", 99.84 → "99.8",
 * 99.96 → "100", 55 → "55". The project-wide rule for every user-facing
 * % text (backlog 9042b47c): render as `${fmtPct(x)}%`. Never emits a
 * trailing ".0" or "-0". Pure.
 */
export function fmtPct(p: number): string {
  const r = round1(p);
  if (r === 0) return "0"; // never render "-0"
  return r.toLocaleString("en-US", { maximumFractionDigits: 1 });
}

/**
 * Format an elapsed duration in milliseconds the way Claude's UI does:
 * "22s" under a minute, "1m 3s" under an hour, "1h 2m 3s" beyond (hour
 * shown, minutes always shown once an hour has passed, seconds always
 * shown). Used by the inflight bar's live turn timer.
 */
export function fmtDuration(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const sec = totalSec % 60;
  const min = Math.floor(totalSec / 60) % 60;
  const hr = Math.floor(totalSec / 3600);
  if (hr > 0) {
    return `${hr}h ${min}m ${sec}s`;
  }
  if (min > 0) {
    return `${min}m ${sec}s`;
  }
  return `${sec}s`;
}
