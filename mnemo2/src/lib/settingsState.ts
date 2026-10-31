import type { EndpointDetailDto, TierName, TiersDto } from "./settingsEngine";

export const TIER_META: Record<TierName, { label: string; blurb: string }> = {
  difficult: { label: "Difficult", blurb: "Design, debugging, bug fixes, shared contracts" },
  average: { label: "Average", blurb: "Typical implementation steps" },
  simple: { label: "Simple and tools", blurb: "Single-file edits, checks, lookups, scaffolding" },
};

export interface EndpointPreset {
  id: string;
  label: string;
  name: string;
  kind: string;
  baseUrl: string;
}

export const ENDPOINT_PRESETS: readonly EndpointPreset[] = [
  { id: "anthropic", label: "Anthropic", name: "anthropic", kind: "anthropic", baseUrl: "https://api.anthropic.com/" },
  { id: "openai", label: "OpenAI", name: "openai", kind: "openai", baseUrl: "https://api.openai.com/v1/" },
  { id: "openrouter", label: "OpenRouter", name: "openrouter", kind: "openai", baseUrl: "https://openrouter.ai/api/v1/" },
  { id: "local", label: "Local / OpenAI-compatible", name: "local", kind: "local", baseUrl: "http://localhost:11434/v1/" },
];

export const ENDPOINT_KINDS: readonly { value: string; label: string }[] = [
  { value: "openai", label: "OpenAI-compatible" },
  { value: "anthropic", label: "Anthropic" },
  { value: "local", label: "Local" },
];

export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

export function specOf(endpoint: string, model: string): string {
  return `${endpoint}/${model}`;
}

/** Every configured `endpoint/model` spec with its price, in endpoint order. */
export function allSpecs(endpoints: EndpointDetailDto[]): { spec: string; input: number | null; output: number | null }[] {
  return endpoints.flatMap((endpoint) =>
    endpoint.models.map((model) => ({
      spec: specOf(endpoint.name, model.id),
      input: model.price?.input_per_1m ?? null,
      output: model.price?.output_per_1m ?? null,
    })),
  );
}

export function formatUsd(value: number): string {
  return `$${Number(value.toFixed(4))}`;
}

/** "$in / $out", or an em dash when the model has no price. */
export function formatPrice(input: number | null, output: number | null): string {
  if (input == null && output == null) return "—";
  return `${formatUsd(input ?? 0)} / ${formatUsd(output ?? 0)}`;
}

export function priceOfSpec(endpoints: EndpointDetailDto[], spec: string): string {
  const found = allSpecs(endpoints).find((entry) => entry.spec === spec);
  return found ? formatPrice(found.input, found.output) : "—";
}

/** Labels of the tiers that list this spec. */
export function tiersListing(tiers: TiersDto, spec: string): string[] {
  return (Object.keys(TIER_META) as TierName[])
    .filter((name) => tiers[name].includes(spec))
    .map((name) => TIER_META[name].label);
}

export function moveItem<T>(items: T[], index: number, delta: number): T[] {
  const target = index + delta;
  if (target < 0 || target >= items.length) return items;
  const next = items.slice();
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

export function cloneTiers(tiers: TiersDto): TiersDto {
  return { difficult: [...tiers.difficult], average: [...tiers.average], simple: [...tiers.simple] };
}
