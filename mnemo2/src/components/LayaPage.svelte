<script lang="ts">
  import { modelRefOptions } from "../lib/modelSettings";
  import {
    DISPATCH_TIERS,
    LAYA_FLAG_META,
    LAYA_STATUS_META,
    ROUTING_LANES,
    evalDateText,
    layaStatusHint,
    leavesPage,
    modeAvailable,
    modeUnlockText,
    routingDraft,
    routingWire,
    shareText,
    type DispatchRoutingSettings,
    type LayaEvent,
    type LayaFlags,
    type LayaStatus,
    type LayaTransport,
    type RoutingDraft,
    type RoutingEvalWire,
    type RoutingLogSummary,
    type RoutingModeName,
    type RoutingStatusWire,
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

  /** The Mnemo2 `[routing]` table, the log summary and the last eval record. */
  let dispatch = $state<RoutingStatusWire | null>(null);
  let dispatchDraft = $state<DispatchRoutingSettings | null>(null);
  let dispatchSaving = $state(false);
  let dispatchError = $state("");
  let dispatchSaved = $state(false);
  let evalRunning = $state(false);
  let evalResult = $state<RoutingEvalWire | null>(null);
  let evalError = $state("");
  /** The page's own root element — the boundary a leaving click starts beyond. */
  let pageEl = $state<HTMLDivElement | null>(null);

  /** The four dispatch modes in display order (design §4.5). */
  const MODES: readonly { mode: RoutingModeName; label: string; blurb: string }[] = [
    { mode: "off", label: "Off", blurb: "Never asks Laya: the lead's tier is required and runs, as today." },
    { mode: "shadow", label: "Shadow", blurb: "Asks Laya and logs its pick; the worker still runs on the lead's tier." },
    { mode: "suggest", label: "Suggest", blurb: "A dispatch with tier \"auto\" runs on Laya's pick when it clears the gate, and is refused when it does not." },
    { mode: "auto", label: "Auto", blurb: "Like suggest, with the fallback tier for a dispatch Laya does not clear." },
  ];

  /** The dispatch draft differs from what is saved. */
  const dispatchDirty = $derived(
    dispatch !== null && dispatchDraft !== null && different(dispatchDraft, dispatch.settings),
  );
  const summary = $derived(dispatch?.summary ?? null);
  const evalRecord = $derived(dispatch?.eval ?? null);
  /** The eval's two runs as the template renders them. */
  const evalTables = $derived(
    evalResult === null
      ? []
      : [
          { title: "Three labels", report: evalResult.three },
          { title: "Two labels", report: evalResult.two },
        ],
  );

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
    return { cheap: "", routine: "", capable: "", lane_medium: "", lane_high: "", escalate: "", threshold: "0.8", enforce: false };
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
    void loadDispatch();
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

  /** Adopt a routing snapshot; unsaved edits keep the draft. */
  function adoptDispatch(snapshot: RoutingStatusWire): void {
    const dirty = dispatch !== null && dispatchDraft !== null && different(dispatchDraft, dispatch.settings);
    dispatch = snapshot;
    if (dispatchDraft === null || !dirty) {
      dispatchDraft = structuredClone(snapshot.settings);
    }
  }

  /** Read the routing snapshot (the page's second load leg). */
  async function loadDispatch(): Promise<void> {
    try {
      adoptDispatch(await transport.routingStatus());
    } catch (failure) {
      dispatchError = errorText(failure);
    }
  }

  /** Pick an unlocked mode and save it at once. */
  function selectMode(mode: RoutingModeName): void {
    if (dispatchDraft === null || summary === null || !modeAvailable(mode, summary)) return;
    void saveMode(mode);
  }

  /** Force a locked mode on, behind a confirm that names the missing record. */
  function forceMode(mode: RoutingModeName): void {
    if (dispatchDraft === null || summary === null) return;
    if (modeAvailable(mode, summary)) return;
    const progress = modeUnlockText(mode, summary);
    if (!confirm(`${mode} mode unlocks at 30 labelled decisions (${progress}). Force it on anyway?`)) return;
    void saveMode(mode);
  }

  /**
   * Save the picked mode at once: the payload rides the saved settings with
   * only the mode swapped, so other unsaved draft edits stay unsaved. A
   * failed save reverts the radio to the saved mode and shows the error.
   */
  async function saveMode(mode: RoutingModeName): Promise<void> {
    if (dispatch === null || dispatchDraft === null || dispatchSaving) return;
    const savedMode = dispatch.settings.mode;
    dispatchDraft.mode = mode;
    dispatchSaved = false;
    const landed = await saveDispatch({
      ...($state.snapshot(dispatch.settings) as DispatchRoutingSettings),
      mode,
    });
    if (!landed) dispatchDraft.mode = savedMode;
  }

  /**
   * Save the routing table. Without a payload the whole draft goes out and a
   * successful save makes the returned snapshot the truth: the draft takes it
   * verbatim, so "saved" and "clean" agree from here on. With a payload (a
   * single-field save, e.g. a picked mode) the other unsaved draft edits stay
   * in the draft. Resolves whether the save landed.
   */
  async function saveDispatch(payload?: DispatchRoutingSettings): Promise<boolean> {
    if (dispatchDraft === null || dispatchSaving) return false;
    const wire = payload ?? ($state.snapshot(dispatchDraft) as DispatchRoutingSettings);
    dispatchSaving = true;
    dispatchError = "";
    try {
      const snapshot = await transport.saveRoutingSettings(wire);
      dispatch = snapshot;
      if (payload === undefined) {
        dispatchDraft = structuredClone(snapshot.settings);
      }
      dispatchSaved = true;
      return true;
    } catch (failure) {
      dispatchError = errorText(failure);
      return false;
    } finally {
      dispatchSaving = false;
    }
  }

  /**
   * While dispatch edits are unsaved, ask before leaving the page. Window
   * unload rides the browser's own prompt; in-app navigation (the settings
   * rail, the dialog's close button or backdrop, Escape) rides capture-phase
   * listeners on the window, so a declined confirm cancels the event before
   * the dialog's own handlers ever run.
   */
  $effect(() => {
    // Tracked in the body: the guard re-arms when the dirty flag flips or the
    // root element binds; the callback reads only the captured root.
    const root = pageEl;
    if (!dispatchDirty) return;
    const guard = (event: Event): void => {
      if (event.type === "beforeunload") {
        // The browser's own "changes you made may not be saved" prompt.
        event.preventDefault();
        (event as BeforeUnloadEvent).returnValue = true;
        return;
      }
      if (!leavesPage(event, root)) return;
      if (confirm("Discard unsaved dispatch routing edits?")) return;
      event.preventDefault();
      event.stopPropagation();
    };
    window.addEventListener("beforeunload", guard);
    window.addEventListener("click", guard, true);
    window.addEventListener("keydown", guard, true);
    return () => {
      window.removeEventListener("beforeunload", guard);
      window.removeEventListener("click", guard, true);
      window.removeEventListener("keydown", guard, true);
    };
  });

  function discardDispatch(): void {
    if (dispatch === null) return;
    dispatchDraft = structuredClone(dispatch.settings);
    dispatchError = "";
    dispatchSaved = false;
  }

  /** Write one tier's default effort; an empty value clears it. */
  function setEffort(tier: string, value: string): void {
    if (dispatchDraft === null) return;
    if (value.trim() === "") delete dispatchDraft.effort[tier];
    else dispatchDraft.effort[tier] = value.trim();
    dispatchSaved = false;
  }

  async function runEval(): Promise<void> {
    evalRunning = true;
    evalError = "";
    try {
      evalResult = await transport.runRoutingEval();
      adoptDispatch(await transport.routingStatus());
    } catch (failure) {
      evalError = errorText(failure);
    } finally {
      evalRunning = false;
    }
  }

  /** Save the eval's suggested gates through the normal save path. */
  async function applySuggestion(): Promise<void> {
    if (dispatchDraft === null || evalResult === null) return;
    if (evalResult.record.min_confidence_3 !== null) {
      dispatchDraft.min_confidence_3 = evalResult.record.min_confidence_3;
    }
    if (evalResult.record.min_confidence_2 !== null) {
      dispatchDraft.min_confidence_2 = evalResult.record.min_confidence_2;
    }
    await saveDispatch();
  }

  /** A count map as "shadow 12 · suggest 3" (empty is "none"). */
  function summaryText(counts: Record<string, number>): string {
    const parts = Object.entries(counts)
      .filter(([, count]) => count > 0)
      .map(([key, count]) => `${key} ${count}`);
    return parts.length === 0 ? "none" : parts.join(" · ");
  }

  /** The per-mode decision counts from the loaded summary. */
  function decisionsText(counts: RoutingLogSummary): string {
    return summaryText(counts.decisions_by_mode);
  }
</script>

<datalist id="laya-page-refs">
  {#each options as option (option)}
    <option value={option}></option>
  {/each}
</datalist>

<div class="laya-page" bind:this={pageEl}>
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

      <section class="group" aria-label="Worker dispatch routing">
        <header>
          <h3>Worker dispatch routing (Mnemo2)</h3>
          <p class="micro">
            One Laya question per dispatch, on the step spec and the lead's brief. Off by default; a mode only logs, suggests or picks the worker's tier at dispatch — it never switches a running session.
          </p>
        </header>
        {#if dispatchDraft === null || summary === null}
          <p class="micro">Loading…</p>
        {:else}
          <div class="mode-list">
            {#each MODES as entry (entry.mode)}
              <div class="mode-row">
                <label class="lever-row">
                  <input
                    type="radio"
                    name="dispatch-mode"
                    value={entry.mode}
                    checked={dispatchDraft.mode === entry.mode}
                    disabled={!modeAvailable(entry.mode, summary)}
                    onchange={() => selectMode(entry.mode)}
                  />
                  <span class="lever-text">
                    <strong>{entry.label}</strong>
                    <span class="micro">{entry.blurb}</span>
                  </span>
                </label>
                {#if !modeAvailable(entry.mode, summary)}
                  <button class="force-link" type="button" onclick={() => forceMode(entry.mode)}>
                    force ({modeUnlockText(entry.mode, summary)})
                  </button>
                {/if}
              </div>
            {/each}
          </div>

          <div class="knob-list">
            <div class="knob-row">
              <label for="dispatch-gate-3">Three-label gate</label>
              <span class="micro">Applies only at confidence ≥ the gate. {evalDateText(evalRecord)}.</span>
              <input id="dispatch-gate-3" type="number" min="0" max="1" step="0.025" bind:value={dispatchDraft.min_confidence_3} />
            </div>
            <div class="knob-row">
              <label for="dispatch-gate-2">Two-label gate</label>
              <span class="micro">Used when only two tiers have a target. {evalDateText(evalRecord)}.</span>
              <input id="dispatch-gate-2" type="number" min="0" max="1" step="0.025" bind:value={dispatchDraft.min_confidence_2} />
            </div>
            <div class="knob-row">
              <label for="dispatch-fallback">Auto-mode fallback tier</label>
              <span class="micro">What auto runs when Laya does not clear the gate.</span>
              <select id="dispatch-fallback" bind:value={dispatchDraft.fallback_tier}>
                {#each DISPATCH_TIERS as tier (tier)}
                  <option value={tier}>{tier}</option>
                {/each}
              </select>
            </div>
            <div class="knob-row">
              <label for="dispatch-min-words">Fewest words to classify</label>
              <span class="micro">Shorter dispatches skip Laya entirely.</span>
              <input id="dispatch-min-words" type="number" min="0" step="1" bind:value={dispatchDraft.min_words} />
            </div>
          </div>

          <label class="lever-row enforce-row">
            <input type="checkbox" bind:checked={dispatchDraft.nudges} />
            <span class="lever-text">
              <strong>Nudges</strong>
              <span class="micro">When a worker looks stuck, Laya suggests a tier once — it never changes the model.</span>
            </span>
          </label>

          <div class="effort-list">
            {#each DISPATCH_TIERS as tier (tier)}
              <div class="knob-row">
                <label for={`dispatch-effort-${tier}`}>{tier} effort</label>
                <input
                  id={`dispatch-effort-${tier}`}
                  value={dispatchDraft.effort[tier] ?? ""}
                  placeholder="default"
                  spellcheck="false"
                  oninput={(event) => setEffort(tier, event.currentTarget.value)}
                />
              </div>
            {/each}
          </div>

          <dl class="summary-list">
            <div><dt>Decisions</dt><dd>{decisionsText(summary)}</dd></div>
            <div><dt>Agreement</dt><dd>{shareText(summary.agreement_rate)}</dd></div>
            <div><dt>Laya accuracy</dt><dd>{shareText(summary.laya_accuracy)}</dd></div>
            <div><dt>Lead accuracy</dt><dd>{shareText(summary.lead_accuracy)}</dd></div>
            <div><dt>Labelled</dt><dd>{summary.labelled}</dd></div>
            <div><dt>Overrides</dt><dd>{summaryText(summary.overrides_by_source)}</dd></div>
            <div><dt>Nudges</dt><dd>{summary.nudges} ({summary.nudges_acted_on} acted on)</dd></div>
          </dl>

          <div class="section-actions">
            {#if dispatchError}<p class="section-error" role="alert">{dispatchError}</p>{/if}
            {#if dispatchSaved && !dispatchDirty}<p class="section-note">Saved to mnemo2.toml.</p>{/if}
            <div class="section-buttons">
              <button class="button button-quiet" disabled={dispatchSaving || !dispatchDirty} onclick={discardDispatch}>Discard</button>
              <button
                class="button button-primary"
                disabled={dispatchSaving || !dispatchDirty}
                onclick={() => void saveDispatch()}
              >
                {dispatchSaving ? "Saving…" : "Save dispatch routing"}
              </button>
            </div>
          </div>

          <div class="eval-block">
            <div class="section-buttons">
              <button class="button button-quiet" disabled={evalRunning} onclick={runEval}>
                {evalRunning ? "Running eval…" : "Run eval"}
              </button>
              {#if evalResult !== null}
                <button class="button button-primary" disabled={dispatchSaving} onclick={applySuggestion}>
                  Apply suggested gates
                </button>
              {/if}
            </div>
            {#if evalError}<p class="section-error" role="alert">{evalError}</p>{/if}
            {#if evalResult !== null}
              {#each evalTables as table (table.title)}
                <div class="eval-report">
                  <h4>
                    {table.title} — suggested
                    {table.report.suggested === null ? "nothing (the provisional default stands)" : table.report.suggested.toFixed(3)}
                  </h4>
                  <table>
                    <thead>
                      <tr><th>Gate</th><th>Routed</th><th>Accuracy</th></tr>
                    </thead>
                    <tbody>
                      {#each table.report.table as row (row.threshold)}
                        <tr>
                          <td>{row.threshold.toFixed(3)}</td>
                          <td>{Math.round(row.routed_share * 100)}%</td>
                          <td>{Math.round(row.accuracy * 100)}%</td>
                        </tr>
                      {/each}
                    </tbody>
                  </table>
                  {#if table.report.misses.length > 0}
                    <p class="micro">Misses at {table.report.default_gate.toFixed(3)}: {table.report.misses.length}</p>
                    <ul class="miss-list">
                      {#each table.report.misses.slice(0, 5) as miss (miss.text)}
                        <li class="micro">
                          want {miss.expected ?? "no route"} · got {miss.got ?? "no answer"}{miss.confidence === null ? "" : ` (${miss.confidence.toFixed(2)})`} — {miss.text.slice(0, 90)}
                        </li>
                      {/each}
                    </ul>
                  {/if}
                </div>
              {/each}
            {/if}
          </div>
        {/if}
      </section>

      <section class="group" aria-label="Routing lanes">
        <header>
          <h3>Mnemo1: first-prompt routing and step lanes (not used by Mnemo2 workers)</h3>
          <p class="micro">
            The models a confident Mnemo1 decision routes to, as "endpoint/model". The routine lane serves the shared three-label question's middle rung; the step lanes ride the Reflex opt-in. Empty lanes keep the tier the lead picks. Mnemo2 workers never read this block — their dispatch routing is the table above.
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

  .mode-list { display: grid; gap: 6px; }
  .mode-row { display: grid; gap: 4px; }
  .force-link {
    justify-self: start;
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    cursor: pointer;
    font-size: var(--text-body);
    text-decoration: underline;
  }

  .effort-list { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px 18px; }

  .summary-list { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 6px 18px; margin: 0; }
  .summary-list div { display: grid; gap: 1px; }
  .summary-list dt { color: var(--muted); font-size: var(--text-body); }
  .summary-list dd { margin: 0; font-size: var(--text-body); font-weight: 620; }

  .eval-block { display: grid; gap: 8px; padding-top: 8px; border-top: 1px solid var(--line); }
  .eval-report { display: grid; gap: 6px; }
  .eval-report h4 { margin: 0; font-size: var(--text-body); font-weight: 620; }
  .eval-report table { border-collapse: collapse; font-size: var(--text-body); }
  .eval-report th,
  .eval-report td { padding: 2px 10px 2px 0; text-align: left; }
  .eval-report th { color: var(--muted); font-weight: 600; }
  .miss-list { margin: 0; padding-left: 18px; }
</style>