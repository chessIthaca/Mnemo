// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { CheckCircle, Circle } from "lucide-react";
import { stepHeadline } from "../../lib/planSteps";
import type { PlanFile } from "../../lib/types";

/**
 * The StatusBar "Executing x/y" popup: the current step's headline ONLY —
 * a single CSS-truncated line, never the recipe body (backlog af572504).
 *
 * The full step list lives in the plan window; this popup is a glance at
 * what is executing right now. The current step is the first not-done
 * step; when every step is done it shows "All steps complete" instead.
 * The full (untruncated) headline rides the title attribute so the CSS
 * truncation stays discoverable on hover. The `step x/y` number derives
 * from the current step's index, not the completed count — exact even if
 * steps ever completed out of order (the toolbar's stateLabel agrees
 * because done steps are always a prefix).
 */
export function ExecutingStepPopup({ plan }: { plan: PlanFile }) {
  const total = plan.steps.length;
  const current = plan.steps.find((s) => !s.done);
  return (
    <div className="absolute bottom-full left-0 z-50 mb-1 w-80 rounded-lg border border-border bg-bg-secondary shadow-2xl">
      <div className="border-b border-border px-3 py-2 text-xs font-medium text-slate-300">
        {plan.title}
        <span className="ml-1 font-normal text-slate-500">
          {current ? `· step ${current.index + 1}/${total}` : "· complete"}
        </span>
      </div>
      <div className="flex items-start gap-2 px-3 py-2 text-left text-xs">
        {current ? (
          <>
            <Circle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-slate-500" />
            <span
              className="min-w-0 flex-1 truncate text-slate-200"
              title={stepHeadline(current)}
            >
              {stepHeadline(current)}
            </span>
          </>
        ) : (
          <>
            <CheckCircle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-green-400" />
            <span className="text-slate-400">All steps complete</span>
          </>
        )}
      </div>
    </div>
  );
}
