// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { CheckCircle, ChevronRight, Circle } from "lucide-react";
import { stepBody } from "../../lib/planSteps";
import type { PlanStep } from "../../lib/types";

/**
 * One plan-step row for the plan window: state icon + always-visible
 * headline + collapsible details body (backlog af572504).
 *
 * A step is stored as `**Header** — body`; the header is the headline and
 * `stepBody` (lib/planSteps) strips it to get the details. Headered steps
 * collapse the body behind a rotating-chevron toggle — the parent drives
 * `expanded` (default collapsed; the active step auto-expands). Plain
 * headerless steps have no details to reveal (`stepBody` passes them
 * through unchanged), so they render the full text with no chevron;
 * header-only steps (empty body) render a spacer so headlines stay
 * aligned. Step-state styling (done/current/pending) is unchanged from
 * the previous always-expanded rows.
 */
export function PlanStepRow({
  step,
  expanded,
  onToggle,
}: {
  step: PlanStep;
  expanded: boolean;
  onToggle: () => void;
}) {
  const body = step.header ? stepBody(step.text) : "";
  const hasDetails = body.trim().length > 0;
  return (
    <div
      className={`flex items-start gap-2 rounded px-2 py-1.5 text-[0.875em] ${
        step.done ? "opacity-60" : "bg-bg-tertiary/50"
      }`}
    >
      {step.done ? (
        <CheckCircle className="mt-0.5 h-4 w-4 shrink-0 text-green-400" />
      ) : (
        <Circle className="mt-0.5 h-4 w-4 shrink-0 text-slate-500" />
      )}
      <div className="min-w-0 flex-1">
        {step.header ? (
          <>
            {hasDetails ? (
              <button
                type="button"
                onClick={onToggle}
                aria-expanded={expanded}
                className="flex w-full items-start gap-1 text-left"
                title={expanded ? "Collapse step details" : "Expand step details"}
              >
                <ChevronRight
                  className={`mt-0.5 h-3.5 w-3.5 shrink-0 text-slate-500 transition-transform ${
                    expanded ? "rotate-90" : ""
                  }`}
                />
                <span
                  className={`font-semibold ${
                    step.done ? "text-slate-500 line-through" : "text-slate-100"
                  }`}
                >
                  {step.header}
                </span>
              </button>
            ) : (
              <div className="flex w-full items-start gap-1 text-left">
                {/* Spacer keeps header-only headlines aligned with the
                    chevroned ones. */}
                <span className="mt-0.5 block h-3.5 w-3.5 shrink-0" />
                <span
                  className={`font-semibold ${
                    step.done ? "text-slate-500 line-through" : "text-slate-100"
                  }`}
                >
                  {step.header}
                </span>
              </div>
            )}
            {hasDetails && expanded && (
              <div
                className={`mt-0.5 whitespace-pre-wrap pl-[1.125rem] font-normal ${
                  step.done ? "text-slate-500 line-through" : "text-slate-400"
                }`}
              >
                {body}
              </div>
            )}
          </>
        ) : (
          <span
            className={`whitespace-pre-wrap ${
              step.done ? "text-slate-500 line-through" : "text-slate-200"
            }`}
          >
            {step.text}
          </span>
        )}
      </div>
    </div>
  );
}
