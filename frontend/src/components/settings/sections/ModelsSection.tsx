// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from "react";
import { Plus, X } from "lucide-react";
import { getSettings, saveSettings, errMsg } from "../../../lib/tauri";
import type { EndpointInfo, ModelRefConfig } from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import type { SettingsSectionHandle } from "../types";
import {
  REASONING_EFFORTS,
  REASONING_EFFORT_DEFAULT,
  effortFromSelectValue,
  effortToSelectValue,
} from "../types";

/**
 * Models section — per-context model overrides (the `[models]` config section).
 *
 * Each context (workflow state, plan kind, subagent, or skill) can optionally
 * run on a different endpoint + model than the main agent's default. Unset
 * overrides fall back to the default. Resolution priority at turn time: skill >
 * subagent > bug-fixing plan kind (while the active plan's kind is bug_fixing
 * in Executing/Reviewing) > state > default — except subagents themselves,
 * whose workflow state is a role state (Subagent), never a lifecycle phase: an
 * unset subagent override falls back straight to the default, not to a state
 * model. The summarize slot is a dedicated consumer outside that chain: it
 * routes compaction summaries (auto-compaction + the run-all between-items
 * compact) to its model; unset rides the turn's model.
 */

/** The seven fixed override slots (workflow states + plan kind + subagent +
 *  the dedicated summarize consumer). */
const FIXED_SLOTS = [
  { key: "planning", label: "Planning", hint: "No plan yet — read tools + create_plan" },
  { key: "executing", label: "Executing", hint: "A plan is in flight — all tools" },
  { key: "bug_fixing", label: "Bug fixing", hint: "Active plan kind is bug_fixing — overrides Executing while set" },
  { key: "reviewing", label: "Reviewing", hint: "Spawned reviewer only — main agent keeps Executing" },
  { key: "complete", label: "Complete", hint: "All steps done — read tools only" },
  { key: "subagent", label: "Subagents", hint: "Background agents via spawn_agent; unset falls back to the default (not a state model)" },
  { key: "summarize", label: "Summarization", hint: "Compaction summaries (auto-compact + run-all between-items); unset rides the turn's model" },
] as const;

type FixedKey = (typeof FIXED_SLOTS)[number]["key"];

/** A draft override: null = cleared, {endpoint,model} = set. */
type SlotDraft = ModelRefConfig | null;

interface Draft {
  planning: SlotDraft;
  executing: SlotDraft;
  bug_fixing: SlotDraft;
  reviewing: SlotDraft;
  complete: SlotDraft;
  subagent: SlotDraft;
  summarize: SlotDraft;
  skill: Record<string, ModelRefConfig>;
}

function emptyDraft(): Draft {
  return {
    planning: null,
    executing: null,
    bug_fixing: null,
    reviewing: null,
    complete: null,
    subagent: null,
    summarize: null,
    skill: {},
  };
}

/** Canonicalize a loaded ref: the wire omits an unset reasoning_effort
 *  (skip_serializing_if), but reverting the effort dropdown to "model
 *  default" sets reasoning_effort: null — normalizing on load keeps the
 *  draft and its snapshot stringifying identically, so a net-zero edit
 *  clears dirty like every other field (review LOW 1, 2027-01-07). */
function canonicalRef(ref: ModelRefConfig): ModelRefConfig {
  return { ...ref, reasoning_effort: ref.reasoning_effort ?? null };
}

/** Build the editable draft from the loaded settings. Exported for the
 *  false-dirty regression test (the section itself loads via IPC on mount,
 *  which renderToStaticMarkup cannot drive). */
export function draftFromSettings(s: {
  models: {
    planning: ModelRefConfig | null;
    executing: ModelRefConfig | null;
    bug_fixing: ModelRefConfig | null;
    reviewing: ModelRefConfig | null;
    complete: ModelRefConfig | null;
    subagent: ModelRefConfig | null;
    summarize: ModelRefConfig | null;
    skill: Record<string, ModelRefConfig>;
  };
}): Draft {
  return {
    planning: s.models.planning ? canonicalRef(s.models.planning) : null,
    executing: s.models.executing ? canonicalRef(s.models.executing) : null,
    bug_fixing: s.models.bug_fixing ? canonicalRef(s.models.bug_fixing) : null,
    reviewing: s.models.reviewing ? canonicalRef(s.models.reviewing) : null,
    complete: s.models.complete ? canonicalRef(s.models.complete) : null,
    subagent: s.models.subagent ? canonicalRef(s.models.subagent) : null,
    summarize: s.models.summarize ? canonicalRef(s.models.summarize) : null,
    skill: Object.fromEntries(
      Object.entries(s.models.skill).map(([name, ref]) => [name, canonicalRef(ref)]),
    ),
  };
}

function serializeDraft(d: Draft): string {
  return JSON.stringify(d);
}

export const ModelsSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function ModelsSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const [endpoints, setEndpoints] = useState<EndpointInfo[]>([]);
  const [draft, setDraft] = useState<Draft>(emptyDraft());
  const [snapshot, setSnapshot] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  const [newSkillName, setNewSkillName] = useState("");

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      setEndpoints(s.endpoints ?? []);
      const d = draftFromSettings(s);
      setDraft(d);
      setSnapshot(serializeDraft(d));
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  const dirty = snapshot !== "" && serializeDraft(draft) !== snapshot;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  function setFixed(key: FixedKey, value: SlotDraft) {
    setDraft((d) => ({ ...d, [key]: value }));
  }

  function addSkillOverride(name: string) {
    const n = name.trim();
    if (!n) return;
    setDraft((d) => {
      if (d.skill[n]) return d;
      const ep = endpoints[0];
      return {
        ...d,
        skill: {
          ...d.skill,
          [n]: { endpoint: ep?.name ?? "", model: ep?.models[0] ?? "" },
        },
      };
    });
    setNewSkillName("");
  }

  function updateSkillOverride(name: string, value: ModelRefConfig) {
    setDraft((d) => ({
      ...d,
      skill: { ...d.skill, [name]: value },
    }));
  }

  function removeSkillOverride(name: string) {
    setDraft((d) => {
      const next = { ...d.skill };
      delete next[name];
      return { ...d, skill: next };
    });
  }

  async function handleSave(): Promise<boolean> {
    setSaving(true);
    setError(null);
    setOk(false);
    try {
      // Build the patch. Each fixed slot is sent (null clears, object sets).
      // The skill map is sent in full (replaces).
      await saveSettings({
        models: {
          planning: draft.planning,
          executing: draft.executing,
          bug_fixing: draft.bug_fixing,
          reviewing: draft.reviewing,
          complete: draft.complete,
          subagent: draft.subagent,
          summarize: draft.summarize,
          skill: draft.skill,
        },
      });
      setSnapshot(serializeDraft(draft));
      setOk(true);
      bumpConfigVersion();
      window.setTimeout(() => setOk(false), 2500);
      return true;
    } catch (e) {
      setError(errMsg(e));
      return false;
    } finally {
      setSaving(false);
    }
  }

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({ save: handleSave }));

  const skillNames = useMemo(() => Object.keys(draft.skill).sort(), [draft.skill]);

  if (loading) {
    return (
      <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
        Loading model overrides…
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Models
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Run specific contexts on a different endpoint + model than the default
          (e.g. a fast coding model for subagents, a smart model for planning).
          Unset overrides fall back to the default. Priority: skill &gt; subagent
          &gt; bug-fixing plan kind (while the active plan's kind is bug_fixing
          in Executing/Reviewing) &gt; state &gt; default — subagents skip the
          state tier (their state is a role state, not a lifecycle phase), so an
          unset subagent override falls back straight to the default. The
          Summarization slot sits outside the chain: it routes compaction
          summaries (auto-compact + run-all between-items) to its model; unset
          rides the turn's model.
        </p>
      </div>

      {endpoints.length === 0 && (
        <div className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-300">
          No endpoints configured. Add one in the Providers tab before setting
          overrides.
        </div>
      )}

      <div className="space-y-3">
        {FIXED_SLOTS.map((slot) => (
          <ModelPicker
            key={slot.key}
            label={slot.label}
            hint={slot.hint}
            endpoints={endpoints}
            value={draft[slot.key]}
            onChange={(v) => setFixed(slot.key, v)}
          />
        ))}
      </div>

      <div className="space-y-2">
        <h4 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Per-skill overrides
        </h4>
        <p className="text-xs text-[color:var(--text-muted)]">
          A skill (e.g. <code className="text-[color:var(--accent-color)]">merge_to_main</code>)
          runs on the named model while active. Skills not listed fall back to the
          subagent/state/default chain.
        </p>

        {skillNames.length === 0 && (
          <p className="text-xs text-[color:var(--text-muted)]">
            No skill overrides configured.
          </p>
        )}

        {skillNames.map((name) => (
          <div
            key={name}
            className="space-y-2 rounded-lg border border-border bg-bg-primary px-3 py-2.5"
          >
            <div className="flex items-center justify-between gap-2">
              <code className="text-xs text-[color:var(--accent-color)]">{name}</code>
              <button
                type="button"
                onClick={() => removeSkillOverride(name)}
                className="rounded p-1 text-[color:var(--text-muted)] hover:text-red-300"
                aria-label={`Remove ${name} override`}
                title="Remove override"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
            <ModelPickerBody
              endpoints={endpoints}
              value={draft.skill[name]}
              onChange={(v) => updateSkillOverride(name, v)}
            />
          </div>
        ))}

        <div className="flex items-center gap-2">
          <input
            value={newSkillName}
            onChange={(e) => setNewSkillName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addSkillOverride(newSkillName);
              }
            }}
            placeholder="skill name (e.g. merge_to_main)"
            spellCheck={false}
            className="min-w-0 flex-1 rounded-lg border border-border bg-bg-primary px-3 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
          />
          <button
            type="button"
            onClick={() => addSkillOverride(newSkillName)}
            disabled={!newSkillName.trim()}
            className="flex shrink-0 items-center gap-1 rounded-lg border border-border px-2.5 py-1.5 text-xs text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)] disabled:opacity-40"
          >
            <Plus className="h-3.5 w-3.5" />
            Add
          </button>
        </div>
      </div>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Model overrides saved.
        </div>
      )}
    </div>
  );
});

/**
 * A single override picker with an enable checkbox + endpoint/model dropdowns.
 */
function ModelPicker({
  label,
  hint,
  endpoints,
  value,
  onChange,
}: {
  label: string;
  hint: string;
  endpoints: EndpointInfo[];
  value: SlotDraft;
  onChange: (v: SlotDraft) => void;
}) {
  const enabled = value !== null;
  return (
    <div className="space-y-2 rounded-lg border border-border bg-bg-primary px-3 py-2.5">
      <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) =>
            onChange(
              e.target.checked
                ? {
                    endpoint: endpoints[0]?.name ?? "",
                    model: endpoints[0]?.models[0] ?? "",
                  }
                : null,
            )
          }
          className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        <span className="font-medium">{label}</span>
        <span className="text-[color:var(--text-muted)]">— {hint}</span>
      </label>
      {enabled && (
        <ModelPickerBody endpoints={endpoints} value={value} onChange={onChange} />
      )}
    </div>
  );
}

/**
 * The endpoint + model + effort dropdowns (shared by fixed slots + skill
 * overrides). Exported for the render test — the section itself loads its
 * data via IPC on mount, which renderToStaticMarkup cannot drive.
 */
export function ModelPickerBody({
  endpoints,
  value,
  onChange,
}: {
  endpoints: EndpointInfo[];
  value: ModelRefConfig;
  onChange: (v: ModelRefConfig) => void;
}) {
  const host = endpoints.find((e) => e.name === value.endpoint);
  const effortDisabled = host?.supports_reasoning_effort === false;
  return (
    <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
      <div className="space-y-1">
        <label className="text-[0.7rem] text-[color:var(--text-muted)]">Endpoint</label>
        <select
          value={value.endpoint}
          onChange={(e) => {
            const ep = endpoints.find((x) => x.name === e.target.value);
            onChange({
              ...value,
              endpoint: e.target.value,
              model: ep?.models[0] ?? value.model,
            });
          }}
          className="w-full rounded-lg border border-border bg-bg-primary px-2.5 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        >
          <option value="">— select —</option>
          {endpoints.map((ep) => (
            <option key={ep.name} value={ep.name}>
              {ep.name}
            </option>
          ))}
        </select>
      </div>
      <div className="space-y-1">
        <label className="text-[0.7rem] text-[color:var(--text-muted)]">Model</label>
        {host && host.models.length > 0 ? (
          <select
            value={value.model}
            onChange={(e) => onChange({ ...value, model: e.target.value })}
            className="w-full rounded-lg border border-border bg-bg-primary px-2.5 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
          >
            <option value="">— select —</option>
            {host.models.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
            {value.model && !host.models.includes(value.model) && (
              <option value={value.model}>{value.model} (custom)</option>
            )}
          </select>
        ) : (
          <input
            value={value.model}
            onChange={(e) => onChange({ ...value, model: e.target.value })}
            placeholder="model id"
            spellCheck={false}
            className="w-full rounded-lg border border-border bg-bg-primary px-2.5 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
          />
        )}
      </div>
      {/* Per-context reasoning-effort override: "model default" = inherit
          the model's own default (per-model → endpoint → "max"). Hidden for
          anthropic hosts (the Messages API has no effort field), disabled
          when the endpoint doesn't accept reasoning_effort. */}
      {host && host.kind !== "anthropic" && (
        <div className="space-y-1">
          <label className="text-[0.7rem] text-[color:var(--text-muted)]">Effort</label>
          <select
            value={effortToSelectValue(value.reasoning_effort ?? null)}
            onChange={(e) =>
              onChange({
                ...value,
                reasoning_effort: effortFromSelectValue(e.target.value),
              })
            }
            disabled={effortDisabled}
            title={
              effortDisabled
                ? "This endpoint does not accept reasoning_effort"
                : "Reasoning effort for this context (unset = the model's own default)"
            }
            className="w-full rounded-lg border border-border bg-bg-primary px-2.5 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
          >
            <option value={REASONING_EFFORT_DEFAULT}>model default</option>
            {REASONING_EFFORTS.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </div>
      )}
    </div>
  );
}
