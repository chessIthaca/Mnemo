// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Editable combobox for the Savings section's model field (backlog
 * 82dd66fc, user-reported): a dropdown listing the models available from
 * the configured endpoints (grouped/deduped across endpoints, annotated
 * with the endpoint names — see modelOptions.ts) PLUS free-text entry for
 * models the lists don't carry (new endpoints, stale cache). Selecting an
 * entry fills the field with the model id; typing filters the list; free
 * text passes through unchanged — pricing is keyed by exact model name
 * (Config::pricing_for), so picking from the list prevents the
 * silently-never-applies typo while custom ids stay possible. No forced
 * validation: save behavior is the parent's, unchanged.
 *
 * A11y/focus contracts (review rounds 1-2): the popup is a role="listbox"
 * with role="option" entries and aria-autocomplete="list" on the field
 * (ARIA 1.2 combobox pattern; the empty-filter message renders OUTSIDE the
 * listbox — a listbox's owned children must be options); selecting an
 * entry returns focus to the field (the entry unmounts — a bare unmount
 * would drop focus to <body>) guarded so the programmatic refocus does not
 * reopen the list; and the dropdown closes when focus leaves the combobox
 * root, so at most one dropdown is open across the pricing rows. The entry
 * and toggle buttons prevent the mousedown default (the focus change):
 * WebKit (macOS/WKWebView) does not focus buttons on mousedown, so without
 * the guard the root-blur close would unmount the dropdown before the
 * click dispatches — mouse selection would silently do nothing on macOS.
 * The toggle also pulls focus into the field when clicked from outside the
 * root, so the blur-close is always armed (round 3 F3 — without it, a
 * chevron-opened dropdown could coexist with another row's).
 */

import { useId, useRef, useState, type FocusEvent, type KeyboardEvent } from "react";
import { ChevronDown } from "lucide-react";
import { filterModelOptions, type ModelOption } from "./modelOptions";

interface ModelComboboxProps {
  /** Current model id — controlled; the parent pricing row owns the value. */
  value: string;
  /** Free-typed text AND dropdown selections both flow through here. */
  onChange: (model: string) => void;
  /** Options from buildModelOptions(getSettings().endpoints). */
  options: ModelOption[];
}

export function ModelCombobox({ value, onChange, options }: ModelComboboxProps) {
  const [open, setOpen] = useState(false);
  const listId = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  // One-shot guard: refocusing the field after a selection must not reopen
  // the dropdown via onFocus (programmatic .focus() fires it synchronously,
  // so the flag is set right before focusing and consumed in that handler).
  const skipOpenRef = useRef(false);
  const visible = filterModelOptions(options, value);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Escape" && open) {
      // Close the dropdown without also closing the surrounding dialog.
      e.stopPropagation();
      setOpen(false);
    }
  };

  // Close when focus leaves the combobox root — Tab to the next row, or a
  // click into another row's field (which paints above this row's overlay,
  // so the overlay alone cannot close it). The entries and the toggle live
  // inside the root, so selecting or toggling never trips this; a null
  // relatedTarget (blur to body / an overlay click) closes too.
  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    const to = e.relatedTarget;
    if (!(to instanceof Node) || !rootRef.current?.contains(to)) {
      setOpen(false);
    }
  };

  return (
    <div ref={rootRef} className="relative" onBlur={onBlur}>
      {open && (
        <div
          className="fixed inset-0 z-10"
          onClick={() => setOpen(false)}
          aria-hidden="true"
        />
      )}
      <div className="relative z-20 flex">
        <input
          ref={inputRef}
          type="text"
          role="combobox"
          aria-expanded={open}
          aria-controls={open && visible.length > 0 ? listId : undefined}
          aria-autocomplete="list"
          autoComplete="off"
          spellCheck={false}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onFocus={() => {
            if (skipOpenRef.current) {
              skipOpenRef.current = false;
              return;
            }
            setOpen(true);
          }}
          onKeyDown={onKeyDown}
          className="w-full min-w-0 rounded-l border border-border bg-bg-secondary px-2 py-1 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
        <button
          type="button"
          aria-label="Toggle model list"
          tabIndex={-1}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            setOpen((o) => !o);
            // Pull focus into the field when toggling from outside the root
            // (review round 3 F3): the mousedown guard keeps focus wherever
            // it was, so a chevron-opened dropdown would otherwise sit open
            // without the root-blur close armed — two dropdowns could be
            // open at once. Opening arms the close; closing refocuses the
            // field with the flag consumed (onFocus returns before its
            // setOpen(true), and the toggle's own setOpen stands).
            if (document.activeElement !== inputRef.current) {
              skipOpenRef.current = true;
              inputRef.current?.focus();
            }
          }}
          className="shrink-0 rounded-r border border-l-0 border-border bg-bg-secondary px-1.5 text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)] focus:outline-none"
        >
          <ChevronDown
            className={`h-3.5 w-3.5 transition-transform ${open ? "rotate-180" : ""}`}
          />
        </button>
      </div>
      {open && (
        <div className="absolute left-0 right-0 top-full z-20 mt-1 max-h-48 overflow-y-auto rounded border border-border bg-bg-primary shadow-lg">
          {visible.length === 0 ? (
            <div className="px-2 py-1.5 text-xs text-[color:var(--text-muted)]">
              No configured model matches — free-typed values still save.
            </div>
          ) : (
            <div id={listId} role="listbox">
              {visible.map((o) => (
                <button
                  key={o.model}
                  type="button"
                  role="option"
                  aria-selected={o.model === value}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => {
                    onChange(o.model);
                    setOpen(false);
                    // Return focus to the field — the entry button unmounts
                    // on close, and a bare unmount would drop focus to
                    // <body> (review L2). The guard stops the synchronous
                    // onFocus from reopening the list, and only engages when
                    // focus actually moved (the keyboard path): the mousedown
                    // default is prevented (review round 2 F1 — WebKit does
                    // not focus buttons on mousedown, so the root-blur close
                    // must not unmount the dropdown before the click fires),
                    // which keeps focus on the field for mouse selection — a
                    // no-op focus() would leave a stale flag that swallows
                    // the next genuine open.
                    if (document.activeElement !== inputRef.current) {
                      skipOpenRef.current = true;
                      inputRef.current?.focus();
                    }
                  }}
                  className="flex w-full items-baseline gap-2 px-2 py-1.5 text-left text-xs hover:bg-bg-secondary"
                >
                  <span className="min-w-0 truncate text-[color:var(--text-primary)]">
                    {o.model}
                  </span>
                  <span className="min-w-0 truncate text-[0.65rem] text-[color:var(--text-muted)]">
                    {o.endpoints.join(", ")}
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
