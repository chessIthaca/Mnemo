// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Placeholder for Settings sections not yet fully wired (Safety, Memory,
 * Vision, Advanced). Points users at the current surface for that concern.
 */
export function PlaceholderSection({
  title,
  body,
  bullets,
}: {
  title: string;
  body: string;
  bullets?: string[];
}) {
  return (
    <div className="space-y-3">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
        {title}
      </h3>
      <p className="text-sm text-[color:var(--text-primary)]">{body}</p>
      {bullets && bullets.length > 0 && (
        <ul className="list-inside list-disc space-y-1 text-xs text-[color:var(--text-muted)]">
          {bullets.map((b) => (
            <li key={b}>{b}</li>
          ))}
        </ul>
      )}
      <p className="rounded-lg border border-dashed border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-muted)]">
        Full editor coming in a follow-up — see <code className="text-[color:var(--accent-color)]">grok.md</code> phases P2–P4.
      </p>
    </div>
  );
}
