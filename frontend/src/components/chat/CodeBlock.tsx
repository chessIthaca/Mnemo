// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * A highlighted code block with a language header + copy button.
 *
 * Extracted from `Message.tsx` so the chat transcript and the File viewer
 * share ONE code-block presentation. The `className` comes from
 * rehype-highlight (it prepends `hljs language-*` to the code element's
 * class list); only the `language-*` tag is pulled onto the inline <code> —
 * the `.hljs` box styles (background + padding) must stay on the block <pre>,
 * otherwise the inline padding paints the background over adjacent lines and
 * the lines overlap. Colors come from the `.hljs-*` CSS-custom-property rules
 * in `styles/globals.css` (theme-aware, user-configurable).
 */
import { useState, useRef, type ReactNode } from "react";
import { Check, Copy } from "lucide-react";

/** Render a fenced code block (`children` = the highlighted code nodes). */
export function CodeBlock({
  className,
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  const [copied, setCopied] = useState(false);
  const codeRef = useRef<HTMLElement>(null);
  // rehype-highlight prepends `hljs` to the code element's class list, so
  // className looks like "hljs language-toml". Pull out only the language-*
  // tag for the inline <code>.
  const langClass = className?.split(/\s+/).find((c) => c.startsWith("language-"));
  const lang = langClass ? langClass.slice("language-".length) : "text";

  const copy = () => {
    // children is a React node tree for highlighted blocks; String(children)
    // would yield "[object Object]". The <code> element's textContent is the
    // exact source text, so copy that instead.
    const text = codeRef.current?.textContent ?? String(children);
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="group relative my-2 overflow-hidden rounded-lg border border-border bg-bg-primary">
      <div className="flex items-center justify-between border-b border-border px-3 py-1">
        <span className="text-xs text-slate-500">{lang}</span>
        <button
          onClick={copy}
          className="text-slate-500 opacity-0 transition-opacity group-hover:opacity-100 hover:text-slate-300"
        >
          {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
        </button>
      </div>
      <pre className="hljs overflow-x-auto p-3 text-xs">
        <code ref={codeRef} className={langClass}>
          {children}
        </code>
      </pre>
    </div>
  );
}
