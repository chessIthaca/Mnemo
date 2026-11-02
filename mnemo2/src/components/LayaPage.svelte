<script lang="ts">
  import { modelRefOptions } from "../lib/modelSettings";
  import {
    LAYA_FLAG_META,
    LAYA_STATUS_META,
    ROUTING_LANES,
    layaStatusHint,
    routingDraft,
    routingWire,
    type LayaEvent,
    type LayaFlags,
    type LayaStatus,
    type LayaTransport,
    type RoutingDraft,
  } from "../lib/layaSettings";
  import type { EndpointDetailDto } from "../lib/settingsEngine";
  import { errorText } from "../lib/settingsState";

  /**
   * The Decisions settings page: the Laya classifier's enable state with
   * its install progress, the seven opt-in flags and the routing lanes.
   * The status block owns Enable/Install/Disable — the master `enabled`
   * toggle never rides the settings save — and every other section saves
   * on its own button. Laya is off by default; with it off the engine
   * behaves exactly as before.
   */
  let {
    transport,
    endpoints,
  }: {
    transport: LayaTransport;
    endpoints: EndpointDetailDto[];
  } = $props();

  /** The status snapshot; null until the first read lands. */
  let snapshot = $state<LayaStatus | null>(null);
  let loadError = $state("");

  let flags = $state({ draft: emptyFlags(), original: emptyFlags(), saving: false, error: "", saved: false });
  let routing = $state({ draft: emptyRouting(), original: emptyRouting(), saving: false, error: "", saved: false });

  let setupBusy = $state(false);
  let progress = $state<{ label: string; fraction: number | null } | null>(null);
  let setupError = $state("");

  function emptyFlags(): LayaFlags {
    return {
      auto_type_memories: false,
      failure_triage: false,
      failure_triage_knn: false,
      auto_finetune: false,
      steer_tool_choice: false,
      routing: false,
      reflex: false,
    };
  }

  function emptyRouting(): RoutingDraft {
    return { cheap: "", capable: "", lane_medium: "", lane_high: "", escalate: "", threshold: "0.8", enforce: false };
  }

  const options = $derived(modelRefOptions(endpoints));
  const flagsDirty = $derived(different(flags.draft, flags.original));
  const routingDirty = $derived(different(routing.draft, routing.original));
  /** The setup pipeline runs: the buttons go quiet and the bar may tick. */
  const busy = $derived(setupBusy || snapshot?.status === "installing" || snapshot?.status === "starting");

  /**
   * The status word the status line and hint show. The snapshot is
   * re-read only when the pipeline settles, so while it runs a stale
   * disabled/not_installed/failed snapshot would read "Off" under the
   * ticking bar — the pipeline is underway, so "Installing…" outranks it.
   */
  const shownStatus = $derived(
    (setupBusy || progress !== null) &&
      (snapshot?.status === "disabled" || snapshot?.status === "not_installed" || snapshot?.status === "failed")
      ? "installing"
      : (snapshot?.status ?? "disabled"),
  );

  function different(a: unknown, b: unknown): boolean {
    return JSON.stringify(a) !== JSON.stringify(b);
  }

  $effect(() => {
    let cancelled = false;
    loadError = "";
    transport
      .status()
      .then((status) => {
        if (cancelled) return;
        accept(status);
      })
      .catch((failure) => {
        if (!cancelled) loadError = errorText(failure);
      });
    return () => {
      cancelled = true;
    };
  });

  /** Follow the setup's progress events so the bar and status stay honest. */
  $effect(() => {
    let cancelled = false;
    let stop: (() => void) | null = null;
    void transport
      .onLayaEvent((event) => {
        if (event.step !== "laya_runtime") return;
        if (event.type === "progress") {
          setupBusy = true;
          setupError = "";
          progress = { label: event.label ?? "", fraction: event.fraction ?? null };
        } else if (event.type === "done") {
          setupBusy = false;
          progress = null;
          void load();
        } else {
          setupBusy = false;
          progress = null;
          setupError = event.error ?? "The setup failed";
          void load();
        }
      })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      stop?.();
      stop = null;
    };
  });

  /** Adopt a status snapshot; a dirty draft keeps its unsaved edits. */
  function accept(status: LayaStatus): void {
    snapshot = status;
    loadError = "";
    if (!flagsDirty) {
      flags.draft = { ...status.flags };
      flags.original = { ...status.flags };
    }
    if (!routingDirty) {
      routing.draft = routingDraft(status.routing);
      routing.original = routingDraft(status.routing);
    }
  }

  /** Re-read the status after the pipeline settles (done or failed). */
  async function load(): Promise<void> {
    try {
      accept(await transport.status());
    } catch (failure) {
      loadError = errorText(failure);
    }
  }

  /** Enable (or install): the pipeline streams progress on the event channel. */
  async function setup(): Promise<void> {
    setupError = "";
    setupBusy = true;
    try {
      await transport.setup();
    } catch (failure) {
      setupBusy = false;
      setupError = errorText(failure);
    }
  }

  /** Flip the master toggle off and stop the sidecar, then re-read the status. */
  async function disable(): Promise<void> {
    setupError = "";
    try {
      await transport.disable();
      await load();
    } catch (failure) {
      setupError = errorText(failure);
    }
  }

  async function saveFlags(): Promise<void> {
    await persist(flags, () => transport.save({ ...flags.draft }), () => {
      flags.original = { ...flags.draft };
    });
  }

  async function saveRouting(): Promise<void> {
    const parsed = routingWire(routing.draft);
    if (parsed.error) {
      routing.error = parsed.error;
      return;
    }
    const value = parsed.value;
    await persist(routing, () => transport.save(undefined, value), () => {
      routing.original = { ...routing.draft };
    });
  }

  /** One section save: error and success states are the section's own. */
  async function persist(
    section: { saving: boolean; error: string; saved: boolean },
    write: () => Promise<void>,
    acceptSection: () => void,
  ): Promise<void> {
    section.saving = true;
    section.error = "";
    try {
      await write();
      acceptSection();
      section.saved = true;
    } catch (failure) {
      section.error = errorText(failure);
    } finally {
      section.saving = false;
    }
  }

  function discardFlags(): void {
    flags.draft = { ...flags.original };
    flags.error = "";
    flags.saved = false;
  }

  function discardRouting(): void {
    routing.draft = { ...routing.original };
    routing.error = "";
    routing.saved = false;
  }
</script>

<datalist id="laya-page-refs">
  {#each options as option (option)}
    <option value={option}></option>
  {/each}
</datalist>

<div class="laya-page">
  <div class="laya-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if snapshot === null}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="Decision models (Laya)">
        <header>
          <h3>Decision models (Laya)</h3>
          <p class="micro">
            A small local classifier that picks cheap vs capable models, types memories and triages failures. With Laya off nothing changes.
          </p>
        </header>
        <div class="status-row">
          <span class="status-state">Status: {LAYA_STATUS_META[shownStatus]}</span>
          {#if !snapshot.enabled}
            <button class="button button-primary" disabled={busy} onclick={setup}>{busy ? "Enabling…" : "Enable"}</button>
          {:else if !snapshot.installed}
            <button class="button button-quiet" disabled={busy} onclick={setup}>{busy ? "Installing…" : "Install"}</button>
          {/if}
          {#if snapshot.enabled}
            <button class="button button-quiet" onclick={disable}>Disable</button>
          {/if}
        </div>
        <p class="micro">{layaStatusHint(shownStatus)}</p>
        {#if progress}
          {#if progress.fraction !== null}
            <div
              class="progress"
              role="progressbar"
              aria-label="Install progress"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(progress.fraction * 100)}
            >
              <div style="width: {Math.min(100, Math.round(progress.fraction * 100))}%"></div>
            </div>
          {/if}
          <p class="micro progress-label">{progress.label}</p>
        {/if}
        {#if setupError}
          <p class="section-error" role="alert">{setupError}</p>
        {/if}
        <p class="micro">
          Enabling downloads a self-contained install (uv, a virtualenv, laya[serve] and the English checkpoint) and starts a local server on 127.0.0.1 while the app runs.
        </p>
      </section>

      <section class="group" aria-label="What Laya decides">
        <header>
          <h3>What Laya decides</h3>
          <p class="micro">
            Seven opt-ins, each confidence-gated — a low-confidence or missing answer keeps today's behavior byte-for-byte.
          </p>
        </header>
        <div class="lever-list">
          {#each LAYA_FLAG_META as flag (flag.key)}
            <label class="lever-row">
              <input type="checkbox" bind:checked={flags.draft[flag.key]} />
              <span class="lever-text">
                <strong>{flag.label}</strong>
                <span class="micro">{flag.explanation}</span>
                {#if flag.note}<span class="micro flag-note">{flag.note}</span>{/if}
              </span>
            </label>
          {/each}
        </div>
        <div class="section-actions">
          {#if flags.error}<p class="section-error" role="alert">{flags.error}</p>{/if}
          {#if flags.saved && !flagsDirty}<p class="section-note">Applies to new turns.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={flags.saving || !flagsDirty} onclick={discardFlags}>Discard</button>
            <button class="button button-primary" disabled={flags.saving || !flagsDirty} onclick={saveFlags}>
              {flags.saving ? "Saving…" : "Save flags"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Routing lanes">
        <header>
          <h3>Routing lanes</h3>
          <p class="micro">
            The models a confident decision routes to, as "endpoint/model". Used when the Model routing or Reflex opt-ins are on; empty lanes keep the tier the lead picks.
          </p>
        </header>
        {#each ROUTING_LANES as lane (lane.key)}
          <div class="slot-row">
            <div class="slot-heading">
              <label for={`lane-${lane.key}`}>{lane.label}</label>
              <span class="micro">{lane.blurb}</span>
            </div>
            <div class="slot-input">
              <input
                id={`lane-${lane.key}`}
                list="laya-page-refs"
                bind:value={routing.draft[lane.key]}
                placeholder="endpoint/model"
                spellcheck="false"
              />
              <button
                class="button button-quiet"
                aria-label={`Clear ${lane.label}`}
                disabled={routing.draft[lane.key] === ""}
                onclick={() => (routing.draft[lane.key] = "")}
              >
                Clear
              </button>
            </div>
          </div>
        {/each}
        <div class="knob-list">
          <div class="knob-row">
            <label for="lane-threshold">Confidence threshold</label>
            <span class="micro">A decision applies only at confidence ≥ threshold (0–1).</span>
            <input id="lane-threshold" inputmode="decimal" bind:value={routing.draft.threshold} spellcheck="false" />
          </div>
          <label class="lever-row enforce-row">
            <input type="checkbox" bind:checked={routing.draft.enforce} />
            <span class="lever-text">
              <strong>Enforce decisions</strong>
              <span class="micro">Off = shadow mode: decisions are logged, not applied.</span>
            </span>
          </label>
        </div>
        <div class="section-actions">
          {#if routing.error}<p class="section-error" role="alert">{routing.error}</p>{/if}
          {#if routing.saved && !routingDirty}<p class="section-note">Applies to new turns.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={routing.saving || !routingDirty} onclick={discardRouting}>Discard</button>
            <button class="button button-primary" disabled={routing.saving || !routingDirty} onclick={saveRouting}>
              {routing.saving ? "Saving…" : "Save routing"}
            </button>
          </div>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .laya-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .laya-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }

  .status-row { display: flex; align-items: center; gap: 12px; }
  .status-state { font-size: var(--text-body); color: var(--ink-soft); }
  .status-row .button { min-height: 32px; padding: 5px 10px; }

  .progress { height: 6px; border-radius: 999px; background: var(--surface-soft); overflow: hidden; }
  .progress > div { height: 100%; border-radius: inherit; background: var(--accent); transition: width 0.2s ease; }
  .progress-label { margin-top: -6px; }

  .lever-list { display: grid; gap: 4px; }
  .lever-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    cursor: pointer;
    font-size: var(--text-body);
  }
  .lever-row:has(input:checked) { border-color: var(--accent); background: var(--accent-soft); }
  .lever-text { display: grid; gap: 1px; }
  .lever-text strong { font-weight: 620; }
  .flag-note { font-style: italic; }

  .slot-row { display: grid; gap: 6px; }
  .slot-heading { display: flex; align-items: baseline; gap: 8px; }
  .slot-heading label { font-size: var(--text-body); font-weight: 620; }
  .slot-input { display: flex; gap: 8px; }
  .slot-input input { flex: 1; min-width: 0; padding: 7px 10px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); font-size: var(--text-body); }
  .slot-input .button { flex-shrink: 0; min-height: 32px; padding: 5px 10px; }

  .knob-list { display: grid; grid-template-columns: 1fr 1fr; gap: 10px 18px; }
  .knob-row { display: grid; gap: 3px; }
  .knob-row label { font-size: var(--text-body); font-weight: 620; }
  .knob-row input {
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-size: var(--text-body);
  }

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; flex-shrink: 0; }
</style>