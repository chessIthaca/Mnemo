import type { ProjectUsage } from "./types";

/** Total tokens the header reports: cached + uncached input + output. */
export function usageTotalTokens(usage: ProjectUsage): number {
  return usage.cachedTokens + usage.inputTokens + usage.outputTokens;
}

/** Whether some recorded usage has no configured or vendor price. */
export function usageIsPartial(usage: ProjectUsage): boolean {
  return usage.unpricedModels.length > 0;
}

/**
 * Format integer micro-USD as a short USD label.
 *
 * Zero always renders as `$0.00`; sub-cent amounts keep four decimals so a
 * tiny nonzero spend is never displayed as free.
 */
export function formatUsageCost(costMicros: number): string {
  const micros = Number.isFinite(costMicros) ? Math.max(0, Math.trunc(costMicros)) : 0;
  if (micros === 0) return "$0.00";
  const dollars = micros / 1_000_000;
  if (dollars < 0.0001) return "<$0.0001";
  return dollars < 0.01 ? `$${dollars.toFixed(4)}` : `$${dollars.toFixed(2)}`;
}

/** Format a token count with grouped thousands for the usage tooltip. */
export function formatTokenCount(tokens: number): string {
  const count = Number.isFinite(tokens) ? Math.max(0, Math.trunc(tokens)) : 0;
  return count.toLocaleString("en-US");
}
