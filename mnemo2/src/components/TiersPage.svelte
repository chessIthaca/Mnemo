<script lang="ts">
  import { ArrowDown, ArrowUp, X } from "@lucide/svelte";
  import { untrack } from "svelte";
  import type { EndpointDetailDto, SettingsTransport, TiersDto } from "../lib/settingsEngine";
  import { allSpecs, cloneTiers, errorText, LAYA_LABELS, moveItem, priceOfSpec } from "../lib/settingsState";
  import {
    DISPATCH_TIERS,
    LAYA_FLAG_META,
    LAYA_REFLEX_META,
    LAYA_STATUS_META,
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
    type RoutingStatusWire,
    type TierChoice,
    type TiersTopControl,
  } from "../lib/layaSettings";

  /**
   * The Settings > Tiers page (plan 87932c46, design §7): the top control —
   * does Laya help pick the tier — the five-rung ladder editor with its
   * dispatch-text preview, the collapsed routing details and the Laya
   * section at the bottom. One page owns everything about tiers.
   */
  let {
    transport,
    layaTransport,
    endpoints,
    tiers,
    ontiers,
  }: {
    transport: SettingsTransport | null;
    layaTransport: LayaTransport | null;
    endpoints: EndpointDetailDto[];
    tiers: TiersDto;
    ontiers: (next: TiersDto) => void;
  } = $props();

  // ── §7.1 The top control ────────────────────────────────────────────────

  /** The control's own wire: the saved choice plus Laya's readiness. */
  let topControl = $state<TiersTopControl | null>(null);
  /** The full Laya snapshot — the runtime state, the flags and Mnemo1's gates. */
  let snapshot = $state<LayaStatus | null>(null);
  /** The Mnemo2 `[routing]` table, its log summary and the last eval record. */
  let routingStatus = $state<RoutingStatusWire | null>(null);
  let loadError = $state("");
  /** The last setup-pipeline event, while one runs. */
  let progress = $state<LayaEvent | null>(null);
  let setupBusy = $state(false);
  let setupError = $state("");
  let savingChoice = $state(false);

  /** The four answers, ordered by how much they hand to Laya. */
  const CHOICES: readonly {
    choice: TierChoice;
    title: string;
    blurb: string;
    needsLaya: boolean;
  }[] = [
    {
      choice: "off",
      title: "No: the lead picks",
      blurb: "Every dispatch runs on the tier the lead names; nothing is classified.",
      needsLaya: false,
    },
    {
      choice: "shadow",
      title: "Watch only",
      blurb: "Laya answers every dispatch and logs its pick; the lead's tier stands.",
      needsLaya: true,
    },
    {
      choice: "suggest",
      title: "Suggest",
      blurb: "A confident answer becomes the tier the lead sees; the lead can still override.",
      needsLaya: true,
    },
    {
      choice: "auto",
      title: "Auto",
      blurb: "A confident answer dispatches the worker on that tier; the lead is told afterwards.",
      needsLaya: true,
    },
  ];

  /** The saved choice — `off` until the first read lands. */
  const choice = $derived(topControl?.choice ?? "off");

  /** Whether Laya helps with the tier at all (the radio labels and the
   * routing details both follow it). */
  const layaHelps = $derived(choice !== "off");

  /** Laya's coarse readiness — an in-flight setup reads as installing. */
  const shownStatus = $derived(
    (setupBusy || progress !== null) &&
      (topControl === null ||
        topControl.laya_status === "disabled" ||
        topControl.laya_status === "not_installed" ||
        topControl.laya_status === "failed")
      ? "installing"
      : (topControl?.laya_status ?? "disabled"),
  );

  /** The Mnemo1 halves the saved choice writes (the table the backend pins). */
  function choiceHalves(target: TierChoice): { enforce: boolean; laya_routing: boolean } {
    if (target === "off") return { enforce: false, laya_routing: false };
    if (target === "auto") return { enforce: true, laya_routing: true };
    return { enforce: false, laya_routing: true };
  }

  /**
   * A warning when the saved choice cannot run: Laya is not up (install,
   * turn on or retry applies it once it is), or Mnemo1's halves on disk
   * disagree with the choice (a hand-edited config).
   */
  const choiceWarning = $derived.by(() => {
    if (topControl === null) return null;
    if (choice !== "off") {
      if (topControl.laya_status === "installing" || topControl.laya_status === "starting") {
        return `Laya is still coming up — ${CHOICES.find((entry) => entry.choice === choice)?.title.toLowerCase()} applies once it is ready.`;
      }
      if (topControl.laya_status !== "ready") {
        return "This choice needs Laya running — install or turn it on below; the lead's tier stands until then.";
      }
    }
    const expected = choiceHalves(choice);
    if (topControl.mnemo1_enforce !== expected.enforce || topControl.laya_routing !== expected.laya_routing) {
      return "Mnemo1's routing gates on disk do not match this choice — picking the choice again realigns them.";
    }
    return null;
  });

  /** The three Laya answers are disabled only when Laya is not installed. */
  function choiceDisabled(target: TierChoice): boolean {
    if (savingChoice) return true;
    const meta = CHOICES.find((entry) => entry.choice === target);
    return (
      meta?.needsLaya === true &&
      topControl !== null &&
      topControl.laya_status === "not_installed"
    );
  }

  /** Whether a choice has the labelled decisions it unlocks at. */
  function choiceUnlocked(target: TierChoice): boolean {
    if (target === "off" || target === "shadow") return true;
    if (routingStatus === null) return false;
    return modeAvailable(target, routingStatus.summary);
  }

  /** What a locked choice is missing, for its Force link. */
  function choiceUnlockText(target: TierChoice): string {
    if (routingStatus === null) return "no routing log yet";
    return modeUnlockText(target, routingStatus.summary);
  }

  /** Save a choice through the one combined command (design §7.1). */
  async function saveChoice(target: TierChoice): Promise<void> {
    if (layaTransport === null) return;
    savingChoice = true;
    setupError = "";
    try {
      topControl = await layaTransport.saveTopControl(target);
      // The mode is half of the `[routing]` table — re-read it so the
      // routing details draft follows, and refresh the summary behind it.
      await refreshRouting();
    } catch (error) {
      setupError = errorText(error);
    } finally {
      savingChoice = false;
    }
  }

  /** Pick a radio card: one click, one save. */
  async function pickChoice(target: TierChoice): Promise<void> {
    if (choiceDisabled(target) || target === choice) return;
    if (!choiceUnlocked(target)) {
      forceChoice(target);
      return;
    }
    await saveChoice(target);
  }

  /** Force a locked choice on, behind a confirm that names the missing rows. */
  function forceChoice(target: TierChoice): void {
    if (routingStatus === null) return;
    const progress = modeUnlockText(target, routingStatus.summary);
    const title = CHOICES.find((entry) => entry.choice === target)?.title ?? target;
    if (!confirm(`${title} unlocks at 30 labelled decisions (${progress}). Force it on anyway?`)) return;
    void saveChoice(target);
  }

  /** Enable + install/start the managed runtime, then re-read everything. */
  async function startSetup(): Promise<void> {
    if (layaTransport === null || setupBusy) return;
    setupBusy = true;
    setupError = "";
    progress = null;
    try {
      await layaTransport.setup();
    } catch (error) {
      setupError = errorText(error);
    } finally {
      setupBusy = false;
      void refreshAll();
    }
  }

  /** Flip the master toggle off, then re-read everything. */
  async function stopLaya(): Promise<void> {
    if (layaTransport === null) return;
    setupError = "";
    try {
      await layaTransport.disable();
    } catch (error) {
      setupError = errorText(error);
    } finally {
      void refreshAll();
    }
  }

  /** Re-read the two status wires (after events, setups and saves). */
  async function refreshAll(): Promise<void> {
    const active = layaTransport;
    if (active === null) return;
    try {
      const [status, control] = await Promise.all([active.status(), active.topControl()]);
      snapshot = status;
      topControl = control;
    } catch (error) {
      loadError = errorText(error);
    }
  }

  /** Re-read the `[routing]` table + its summary (the routing details draft). */
  async function refreshRouting(): Promise<void> {
    const active = layaTransport;
    if (active === null) return;
    try {
      routingStatus = await active.routingStatus();
    } catch (error) {
      loadError = errorText(error);
    }
  }

  // ── §7.2 The ladder ────────────────────────────────────────────────────

  /** The editable ladder — seeded from the loaded DTO once (a save writes it
   * back through `ontiers`, which hands the page the stored copy). */
  let draft = $state<TiersDto>(cloneTiers(untrack(() => tiers)));
  let ladderSaving = $state(false);
  let ladderError = $state("");
  let ladderSaved = $state(false);

  /** Every configured spec with its price, for the pickers and previews. */
  const specOptions = $derived(allSpecs(endpoints));

  function rungOf(id: string) {
    return draft.tiers.find((rung) => rung.id === id);
  }

  /** The rung an empty tier folds down to — its `resolves_to`, by id. */
  function foldTargetOf(id: string): string | null {
    const rung = rungOf(id);
    if (rung === undefined) return null;
    const target = rungOf(rung.resolves_to);
    return target?.name ?? rung.resolves_to;
  }

  /** Add a spec to a tier; ordered first-in, reorder with the arrows. */
  function addSpec(id: string, spec: string): void {
    const rung = rungOf(id);
    if (rung === undefined || spec === "" || rung.models.includes(spec)) return;
    rung.models.push(spec);
    ladderSaved = false;
  }

  /**
   * Remove a spec. The trivial tier cannot be emptied — it is the default
   * model every empty tier folds down to (the backend refuses the save
   * too; this keeps the draft coherent without waiting for the error).
   */
  function removeSpec(id: string, index: number): void {
    const rung = rungOf(id);
    if (rung === undefined) return;
    if (rung.id === "trivial" && rung.models.length === 1) return;
    rung.models.splice(index, 1);
    ladderSaved = false;
  }

  function moveSpec(id: string, index: number, delta: number): void {
    const rung = rungOf(id);
    if (rung === undefined) return;
    rung.models = moveItem(rung.models, index, delta);
    ladderSaved = false;
  }

  /** Write one rung's default effort; empty clears it back to the model default. */
  function setRungEffort(id: string, value: string): void {
    const rung = rungOf(id);
    if (rung === undefined) return;
    rung.effort = value.trim() === "" ? null : value.trim();
    ladderSaved = false;
  }

  const ladderChanged = $derived(JSON.stringify(draft) !== JSON.stringify(tiers));

  /** Save the ladder; the lowest tier's model is required. */
  async function saveLadder(): Promise<void> {
    if (transport === null) return;
    const next = $state.snapshot(draft) as TiersDto;
    const trivial = next.tiers.find((rung) => rung.id === "trivial");
    if (trivial === undefined || trivial.models.length === 0) {
      ladderError = "The trivial tier must have a model — it is the default model the empty tiers fold down to.";
      ladderSaved = false;
      return;
    }
    ladderSaving = true;
    ladderError = "";
    ladderSaved = false;
    try {
      const stored = await transport.saveTiers(next);
      ontiers(stored);
      ladderSaved = true;
    } catch (error) {
      ladderError = errorText(error);
    } finally {
      ladderSaving = false;
    }
  }

  // ── §7.3 Dispatch text preview ──────────────────────────────────────────

  /** `spec ($in/$out)` — the same shape `team_dispatch` writes; a spec
   * without a price stands bare. */
  function modelWithPrice(spec: string): string {
    const found = specOptions.find((option) => option.spec === spec);
    if (found === undefined || found.input === null || found.output === null) return spec;
    return `${spec} ($${found.input}/$${found.output})`;
  }

  /**
   * The `tier` text `team_dispatch` shows the lead — generated from the
   * draft exactly as the backend generates it from the stored ladder:
   * id (name) - definition: models with prices, effort, escalation note;
   * suggest and auto add the `auto` line (Laya is never offered escalate).
   */
  const dispatchPreview = $derived.by(() => {
    const lines: string[] = [];
    for (const rung of draft.tiers) {
      const list =
        rung.models.length > 0
          ? rung.models.map((spec) => modelWithPrice(spec)).join(", ")
          : foldTargetOf(rung.id) !== null && rung.resolves_to !== rung.id
            ? `no model; folds down to ${rung.resolves_to}`
            : "no model";
      const effort = rung.effort === null || rung.effort === "" ? "" : `; effort ${rung.effort}`;
      const escalation = rung.role === "escalate" ? "; escalation only, never a first pick" : "";
      lines.push(`${rung.id} (${rung.name}) - ${rung.definition}: ${list}${effort}${escalation}`);
    }
    if (choice === "suggest" || choice === "auto") {
      lines.push("auto - let Laya choose from the step spec (never escalate)");
    }
    return lines.join("\n");
  });

  // ── §7.3 Routing details (the collapsed block) ─────────────────────────

  /** The Mnemo2 `[routing]` table draft. */
  let dispatchDraft = $state<DispatchRoutingSettings | null>(null);
  let dispatchSaving = $state(false);
  let dispatchError = $state("");
  let dispatchSaved = $state(false);
  /** Mnemo1's gates draft (the models live on the ladder above). */
  let gatesDraft = $state<RoutingDraft | null>(null);
  let gatesSaving = $state(false);
  let gatesError = $state("");
  let gatesSaved = $state(false);
  /** The opt-in flags draft — the five below plus the reflex in the details. */
  let flagsDraft = $state<LayaFlags | null>(null);
  let flagsSaving = $state(false);
  let flagsError = $state("");
  let flagsSaved = $state(false);
  let evalResult = $state<RoutingEvalWire | null>(null);
  let evalRunning = $state(false);
  let evalError = $state("");

  /** Seed the flags and Mnemo1's gates whenever a fresh snapshot lands. */
  $effect(() => {
    const status = snapshot;
    if (status === null) return;
    flagsDraft = { ...status.flags };
    gatesDraft = routingDraft(status.routing);
  });

  /** Seed the `[routing]` draft whenever a fresh table lands. */
  $effect(() => {
    const routing = routingStatus;
    if (routing === null) return;
    dispatchDraft = {
      ...routing.settings,
      effort: { ...routing.settings.effort },
    };
  });

  const dispatchDirty = $derived(
    dispatchDraft !== null &&
    routingStatus !== null &&
    JSON.stringify(dispatchDraft) !== JSON.stringify(routingStatus.settings),
  );

  const gatesDirty = $derived(
    gatesDraft !== null &&
    snapshot !== null &&
    JSON.stringify(gatesDraft) !== JSON.stringify(routingDraft(snapshot.routing)),
  );

  const flagsDirty = $derived(
    flagsDraft !== null &&
    snapshot !== null &&
    JSON.stringify(flagsDraft) !== JSON.stringify(snapshot.flags),
  );

  /** Save the `[routing]` table; the response re-seeds the draft. */
  async function saveDispatch(): Promise<boolean> {
    if (layaTransport === null || dispatchDraft === null) return false;
    dispatchSaving = true;
    dispatchError = "";
    try {
      routingStatus = await layaTransport.saveRoutingSettings(
        $state.snapshot(dispatchDraft) as DispatchRoutingSettings,
      );
      dispatchSaved = true;
      return true;
    } catch (error) {
      dispatchError = errorText(error);
      return false;
    } finally {
      dispatchSaving = false;
    }
  }

  function discardDispatch(): void {
    if (routingStatus === null) return;
    dispatchDraft = {
      ...routingStatus.settings,
      effort: { ...routingStatus.settings.effort },
    };
    dispatchError = "";
    dispatchSaved = false;
  }

  /** Write one tier's dispatch effort; an empty value clears it. */
  function setDispatchEffort(tier: string, value: string): void {
    if (dispatchDraft === null) return;
    if (value.trim() === "") delete dispatchDraft.effort[tier];
    else dispatchDraft.effort[tier] = value.trim();
    dispatchSaved = false;
  }

  /** Save Mnemo1's gates — threshold and first-prompt; enforce rides the
   * top control and is carried through unchanged. */
  async function saveGates(): Promise<void> {
    if (layaTransport === null || gatesDraft === null) return;
    const parsed = routingWire(gatesDraft);
    if (parsed.value === undefined) {
      gatesError = parsed.error;
      return;
    }
    gatesSaving = true;
    gatesError = "";
    try {
      await layaTransport.save(undefined, parsed.value);
      await refreshAll();
      gatesSaved = true;
    } catch (error) {
      gatesError = errorText(error);
    } finally {
      gatesSaving = false;
    }
  }

  function discardGates(): void {
    if (snapshot === null) return;
    gatesDraft = routingDraft(snapshot.routing);
    gatesError = "";
    gatesSaved = false;
  }

  /** Save the opt-in flags (the Laya section's five, or the reflex). */
  async function saveFlags(next: LayaFlags): Promise<void> {
    if (layaTransport === null) return;
    flagsSaving = true;
    flagsError = "";
    try {
      await layaTransport.save($state.snapshot(next) as LayaFlags);
      await refreshAll();
      flagsSaved = true;
    } catch (error) {
      flagsError = errorText(error);
    } finally {
      flagsSaving = false;
    }
  }

  /** Toggle the reflex (Mnemo1's plan-step tiers opt-in) in one save. */
  function toggleReflex(next: boolean): void {
    if (flagsDraft === null) return;
    flagsDraft.reflex = next;
    void saveFlags($state.snapshot(flagsDraft) as LayaFlags);
  }

  async function runEval(): Promise<void> {
    if (layaTransport === null) return;
    evalRunning = true;
    evalError = "";
    try {
      evalResult = await layaTransport.runRoutingEval();
      await refreshRouting();
    } catch (error) {
      evalError = errorText(error);
    } finally {
      evalRunning = false;
    }
  }

  /** The eval's suggested gates, applied through the normal save path. */
  async function applySuggestion(): Promise<void> {
    if (dispatchDraft === null || evalResult === null) return;
    const record = evalResult.record;
    if (record.min_confidence_4 !== null) dispatchDraft.min_confidence_4 = record.min_confidence_4;
    if (record.min_confidence_3 !== null) dispatchDraft.min_confidence_3 = record.min_confidence_3;
    if (record.min_confidence_2 !== null) dispatchDraft.min_confidence_2 = record.min_confidence_2;
    await saveDispatch();
  }

  const evalTables = $derived(
    evalResult === null
      ? []
      : [
          { title: "Four labels", report: evalResult.four },
          { title: "Three labels", report: evalResult.three },
          { title: "Two labels", report: evalResult.two },
        ],
  );

  /** A count map as "shadow 12 · suggest 3" (empty is "none"). */
  function summaryText(counts: Record<string, number>): string {
    const parts = Object.entries(counts)
      .filter(([, count]) => count > 0)
      .map(([key, count]) => `${key} ${count}`);
    return parts.length === 0 ? "none" : parts.join(" · ");
  }

  /** The per-mode decision counts from the loaded summary. */
  function decisionsText(counts: Record<string, number>): string {
    return summaryText(counts);
  }

  // ── Loading, events and leaving with unsaved edits ─────────────────────

  /** Load everything once per transport (the dialog keeps it open). */
  $effect(() => {
    const active = layaTransport;
    if (active === null) return;
    let cancelled = false;
    loadError = "";
    Promise.all([active.status(), active.topControl(), active.routingStatus()])
      .then(([status, control, routing]) => {
        if (cancelled) return;
        snapshot = status;
        topControl = control;
        routingStatus = routing;
      })
      .catch((error) => {
        if (!cancelled) loadError = errorText(error);
      });
    const unsubscribe = active.onLayaEvent((event) => {
      progress = event;
      if (event.type === "failed" && event.error != null) setupError = event.error;
      if (event.type === "done" || event.type === "failed") void refreshAll();
    });
    return () => {
      cancelled = true;
      void unsubscribe.then((stop) => stop());
    };
  });

  /** Any draft the guard should ask about before the page is left. */
  function hasUnsaved(): boolean {
    return (
      ladderChanged ||
      dispatchDirty ||
      gatesDirty ||
      flagsDirty
    );
  }

  let pageEl = $state<HTMLDivElement | null>(null);

  /**
   * While edits are unsaved, ask before leaving the page. Window unload
   * rides the browser's own prompt; in-app navigation (the settings rail,
   * the dialog's close button or backdrop, Escape) rides capture-phase
   * listeners on the window, so a declined confirm cancels the event
   * before the dialog's own handlers ever run.
   */
  $effect(() => {
    const root = pageEl;
    if (!hasUnsaved()) return;
    const guard = (event: Event): void => {
      if (event.type === "beforeunload") {
        // The browser's own "changes you made may not be saved" prompt.
        event.preventDefault();
        (event as BeforeUnloadEvent).returnValue = true;
        return;
      }
      if (!leavesPage(event, root)) return;
      if (confirm("Discard unsaved Tiers edits?")) return;
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
</script>

<datalist id="effort-options">
  <option value="low"></option>
  <option value="medium"></option>
  <option value="high"></option>
</datalist>

<div class="tiers-page" bind:this={pageEl}>
  <div class="tiers-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if layaTransport === null}
      <p class="load-note">Laya settings are available in the desktop app.</p>
    {/if}

    <!-- (a) The top control: does Laya help pick the tier? -->
    <section class="group" aria-label="Does Laya help pick the tier?">
      <header>
        <h3>Does Laya help pick the tier?</h3>
        <p class="micro">
          One choice drives both apps — Mnemo2's dispatch routing and Mnemo1's first-prompt and step-lane routing.
        </p>
      </header>
      <div class="choice-grid" role="radiogroup" aria-label="Who picks the tier">
        {#each CHOICES as entry (entry.choice)}
          <label class="choice-card" class:selected={choice === entry.choice}>
            <input
              type="radio"
              name="tiers-choice"
              value={entry.choice}
              checked={choice === entry.choice}
              disabled={choiceDisabled(entry.choice)}
              onchange={() => void pickChoice(entry.choice)}
            />
            <span class="choice-title">{entry.title}</span>
            <span class="micro">{entry.blurb}</span>
            {#if entry.choice !== "off" && !choiceUnlocked(entry.choice) && !choiceDisabled(entry.choice)}
              <button class="button button-quiet" onclick={() => forceChoice(entry.choice)}>
                Force on ({choiceUnlockText(entry.choice)})
              </button>
            {/if}
          </label>
        {/each}
      </div>
      {#if layaTransport !== null && topControl !== null}
        <div class="readiness">
          {#if shownStatus === "not_installed"}
            <span class="status-state">Laya is not installed</span>
            <button class="button button-primary" disabled={setupBusy} onclick={startSetup}>
              {setupBusy ? "Installing…" : "Install Laya"}
            </button>
          {:else if shownStatus === "disabled"}
            <span class="status-state">Laya is off</span>
            <button class="button button-primary" disabled={setupBusy} onclick={startSetup}>
              {setupBusy ? "Turning on…" : "Turn on"}
            </button>
          {:else if shownStatus === "failed"}
            <span class="status-state">Laya failed to start</span>
            <button class="button button-quiet" disabled={setupBusy} onclick={startSetup}>Retry</button>
            {#if setupError}<span class="micro failure-note">{setupError}</span>{/if}
          {:else if shownStatus === "installing" || shownStatus === "starting"}
            <span class="status-state">{LAYA_STATUS_META[shownStatus]}</span>
          {:else}
            <span class="status-state ok">{topControl.checkpoint ?? "Laya"} ready</span>
            <span class="micro">kNN: {topControl.knn_rows} labelled workers (answers from {topControl.knn_min_rows})</span>
            <button class="button button-quiet" disabled={setupBusy} onclick={stopLaya}>Turn off</button>
          {/if}
        </div>
        {#if progress}
          {#if progress.fraction != null}
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
        {#if setupError && shownStatus !== "failed"}<p class="section-error" role="alert">{setupError}</p>{/if}
        {#if choiceWarning}<p class="section-error" role="alert">{choiceWarning}</p>{/if}
      {/if}
    </section>

    <!-- (b) The ladder -->
    <section class="group" aria-label="Tier ladder">
      <header>
        <h3>Tier ladder</h3>
        <p class="micro">
          Five rungs, cheapest first — the order is fixed. Each tier has a name and a routing definition, an ordered model list and an optional effort. An empty tier folds down to the nearest tier below it; the trivial tier is the default model and cannot be empty.
        </p>
      </header>
      {#each draft.tiers as tier, index (tier.id)}
        <section class="tier-card" data-tier={tier.id} aria-label={`${tier.name} tier`}>
          <header class="tier-head">
            <label class="tier-name" for={`tier-name-${tier.id}`}>
              <span class="micro">Tier</span>
              <input id={`tier-name-${tier.id}`} bind:value={tier.name} spellcheck="false" />
            </label>
            {#if layaHelps && LAYA_LABELS[tier.id]}
              <span class="laya-label" title={`Laya answers this tier as “${LAYA_LABELS[tier.id]}”`}>Laya: {LAYA_LABELS[tier.id]}</span>
            {/if}
          </header>
          <label class="tier-definition" for={`tier-definition-${tier.id}`}>
            <span class="micro">Definition — what the lead and Laya read this tier as</span>
            <textarea
              id={`tier-definition-${tier.id}`}
              bind:value={tier.definition}
              rows={2}
              spellcheck="false"
              placeholder="What kind of work runs on this tier"
            ></textarea>
          </label>
          <div class="model-list">
            {#each tier.models as spec, modelIndex (spec)}
              <div class="model-row">
                <code>{spec}</code>
                <span class="micro">{priceOfSpec(endpoints, spec)}</span>
                <span class="row-buttons">
                  <button
                    class="icon-button"
                    aria-label={`Move ${spec} up`}
                    disabled={modelIndex === 0}
                    onclick={() => moveSpec(tier.id, modelIndex, -1)}
                  >
                    <ArrowUp size={13} />
                  </button>
                  <button
                    class="icon-button"
                    aria-label={`Move ${spec} down`}
                    disabled={modelIndex === tier.models.length - 1}
                    onclick={() => moveSpec(tier.id, modelIndex, 1)}
                  >
                    <ArrowDown size={13} />
                  </button>
                  <button
                    class="icon-button"
                    aria-label={`Remove ${spec} from ${tier.name}`}
                    title={tier.id === "trivial" && tier.models.length === 1 ? "The trivial tier must keep a model" : undefined}
                    disabled={tier.id === "trivial" && tier.models.length === 1}
                    onclick={() => removeSpec(tier.id, modelIndex)}
                  >
                    <X size={13} />
                  </button>
                </span>
              </div>
            {/each}
            <select
              aria-label={`Add a model to ${tier.name}`}
              value=""
              onchange={(event) => {
                addSpec(tier.id, event.currentTarget.value);
                event.currentTarget.value = "";
              }}
            >
              <option value="">Add a model…</option>
              {#each specOptions as option (option.spec)}
                <option value={option.spec}>
                  {option.spec} — {option.input === null || option.output === null ? "no price" : `$${option.input}/$${option.output}`}
                </option>
              {/each}
            </select>
          </div>
          <div class="tier-foot">
            <label class="tier-effort" for={`tier-effort-${tier.id}`}>
              <span class="micro">Effort</span>
              <input
                id={`tier-effort-${tier.id}`}
                list="effort-options"
                value={tier.effort ?? ""}
                placeholder="model default"
                spellcheck="false"
                oninput={(event) => setRungEffort(tier.id, event.currentTarget.value)}
              />
            </label>
            {#if tier.id === "trivial"}
              <p class="micro hint">required: the default model</p>
            {:else if tier.role === "escalate"}
              <p class="micro hint">only after a lower tier failed</p>
            {:else if tier.models.length === 0}
              <p class="micro hint">uses {foldTargetOf(tier.id) ?? tier.resolves_to}'s model</p>
            {/if}
          </div>
        </section>
      {/each}
      <div class="section-actions">
        {#if ladderError}<p class="section-error" role="alert">{ladderError}</p>{/if}
        {#if ladderSaved && !ladderChanged}<p class="section-note">Saved to endpoints.toml.</p>{/if}
        <div class="section-buttons">
          <button class="button button-quiet" disabled={ladderSaving || !ladderChanged} onclick={() => (draft = cloneTiers(tiers))}>
            Discard
          </button>
          <button class="button button-primary" disabled={ladderSaving || !ladderChanged} onclick={() => void saveLadder()}>
            {ladderSaving ? "Saving…" : "Save tiers"}
          </button>
        </div>
      </div>
    </section>

    <!-- (c) The dispatch text the lead sees -->
    <section class="group" aria-label="Dispatch tier text preview">
      <header>
        <h3>Dispatch tier text</h3>
        <p class="micro">
          What `team_dispatch` tells the lead about the `tier` parameter — generated from the ladder above (the draft, not the stored copy, so edits preview before saving).
        </p>
      </header>
      <pre class="preview">{dispatchPreview}</pre>
    </section>

    <!-- (d) Routing details — collapsed, and only while Laya helps -->
    {#if layaHelps}
      <details class="routing-details">
        <summary>Routing details</summary>
        <div class="details-body">
          {#if routingStatus !== null && dispatchDraft !== null}
            <section class="group nested" aria-label="Dispatch routing (Mnemo2)">
              <header>
                <h4>Dispatch routing (Mnemo2)</h4>
                <p class="micro">
                  One Laya question per dispatch, on the step spec and the lead's brief. A mode only logs, suggests or picks the worker's tier at dispatch — it never switches a running session.
                </p>
              </header>
              <div class="knob-list">
                <div class="knob-row">
                  <label for="dispatch-gate-4">Four-label gate</label>
                  <span class="micro">The full ladder asks four labels; this gate decides when the answer is trusted.</span>
                  <input id="dispatch-gate-4" inputmode="decimal" bind:value={dispatchDraft.min_confidence_4} spellcheck="false" />
                </div>
                <div class="knob-row">
                  <label for="dispatch-gate-3">Three-label gate</label>
                  <span class="micro">Applies when only three tiers resolve (a reduced ladder).</span>
                  <input id="dispatch-gate-3" inputmode="decimal" bind:value={dispatchDraft.min_confidence_3} spellcheck="false" />
                </div>
                <div class="knob-row">
                  <label for="dispatch-gate-2">Two-label gate</label>
                  <span class="micro">The strictest gate — only a two-label question.</span>
                  <input id="dispatch-gate-2" inputmode="decimal" bind:value={dispatchDraft.min_confidence_2} spellcheck="false" />
                </div>
                <div class="knob-row">
                  <label for="dispatch-min-words">Minimum words</label>
                  <span class="micro">Steps shorter than this are never classified (the answer would be noise).</span>
                  <input id="dispatch-min-words" inputmode="numeric" bind:value={dispatchDraft.min_words} spellcheck="false" />
                </div>
                <div class="knob-row">
                  <label for="dispatch-fallback">Fallback tier</label>
                  <span class="micro">What auto mode runs when no confident answer lands.</span>
                  <select id="dispatch-fallback" bind:value={dispatchDraft.fallback_tier}>
                    {#each DISPATCH_TIERS as id (id)}
                      {#if rungOf(id)}
                        <option value={id}>{rungOf(id)?.name} ({id})</option>
                      {/if}
                    {/each}
                  </select>
                </div>
                <div class="knob-row">
                  <label for="dispatch-knn-rows">kNN answers from</label>
                  <span class="micro">The labelled-worker count the local overlay votes at.</span>
                  <input id="dispatch-knn-rows" inputmode="numeric" bind:value={dispatchDraft.knn_min_rows} spellcheck="false" />
                </div>
                <div class="knob-row">
                  <label for="dispatch-knn-share">kNN vote share</label>
                  <span class="micro">The agreement share a kNN vote needs to clear.</span>
                  <input id="dispatch-knn-share" inputmode="decimal" bind:value={dispatchDraft.knn_min_share} spellcheck="false" />
                </div>
                <label class="lever-row">
                  <input type="checkbox" bind:checked={dispatchDraft.nudges} />
                  <span class="lever-text">
                    <strong>Nudges</strong>
                    <span class="micro">A confident answer may nudge the lead toward its tier without taking the choice.</span>
                  </span>
                </label>
              </div>
              <div class="knob-list">
                {#each DISPATCH_TIERS as tier (tier)}
                  <div class="knob-row">
                    <label for={`dispatch-effort-${tier}`}>{tier} effort</label>
                    <input
                      id={`dispatch-effort-${tier}`}
                      value={dispatchDraft.effort[tier] ?? ""}
                      placeholder="default"
                      spellcheck="false"
                      oninput={(event) => setDispatchEffort(tier, event.currentTarget.value)}
                    />
                  </div>
                {/each}
              </div>
              <dl class="summary-list">
                <div><dt>Decisions</dt><dd>{decisionsText(routingStatus.summary.decisions_by_mode)}</dd></div>
                <div><dt>Agreement</dt><dd>{shareText(routingStatus.summary.agreement_rate)}</dd></div>
                <div><dt>Laya accuracy</dt><dd>{shareText(routingStatus.summary.laya_accuracy)}</dd></div>
                <div><dt>Lead accuracy</dt><dd>{shareText(routingStatus.summary.lead_accuracy)}</dd></div>
                <div><dt>Labelled</dt><dd>{routingStatus.summary.labelled}</dd></div>
                <div><dt>Overrides</dt><dd>{summaryText(routingStatus.summary.overrides_by_source)}</dd></div>
                <div><dt>Nudges</dt><dd>{routingStatus.summary.nudges} ({routingStatus.summary.nudges_acted_on} acted on)</dd></div>
              </dl>
              <div class="section-actions">
                {#if dispatchError}<p class="section-error" role="alert">{dispatchError}</p>{/if}
                {#if dispatchSaved && !dispatchDirty}<p class="section-note">Saved to mnemo2.toml.</p>{/if}
                <div class="section-buttons">
                  <button class="button button-quiet" disabled={dispatchSaving || !dispatchDirty} onclick={discardDispatch}>Discard</button>
                  <button class="button button-primary" disabled={dispatchSaving || !dispatchDirty} onclick={() => void saveDispatch()}>
                    {dispatchSaving ? "Saving…" : "Save dispatch routing"}
                  </button>
                </div>
              </div>
              <div class="eval-block">
                <div class="section-buttons">
                  <button class="button button-quiet" disabled={evalRunning} onclick={() => void runEval()}>
                    {evalRunning ? "Running eval…" : "Run eval"}
                  </button>
                  {#if evalResult !== null}
                    <button class="button button-primary" disabled={dispatchSaving} onclick={() => void applySuggestion()}>
                      Apply suggested gates
                    </button>
                  {/if}
                </div>
                {#if evalError}<p class="section-error" role="alert">{evalError}</p>{/if}
                {#if evalResult !== null}
                  <p class="micro">Last run {evalDateText(evalResult.record)}.</p>
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
            </section>

            {#if gatesDraft !== null && snapshot !== null}
              <section class="group nested" aria-label="First prompt and step lanes (Mnemo1)">
                <header>
                  <h4>First prompt and step lanes (Mnemo1)</h4>
                  <p class="micro">
                    The gates for Mnemo1's own routing — the first prompt of every turn and each plan step (the Reflex opt-in). The models live on the ladder above; `enforce` rides the top control.
                  </p>
                </header>
                <div class="knob-list">
                  <div class="knob-row">
                    <label for="mnemo1-first-prompt">First-prompt gate</label>
                    <span class="micro">Empty = the kernel default; a confident first-prompt answer routes the turn.</span>
                    <input id="mnemo1-first-prompt" inputmode="decimal" bind:value={gatesDraft.firstPrompt} spellcheck="false" />
                  </div>
                  <div class="knob-row">
                    <label for="mnemo1-threshold">Step-lane threshold</label>
                    <span class="micro">A step-lane decision applies only at confidence ≥ threshold (0–1).</span>
                    <input id="mnemo1-threshold" inputmode="decimal" bind:value={gatesDraft.threshold} spellcheck="false" />
                  </div>
                  <label class="lever-row">
                    <input type="checkbox" checked={snapshot.flags.reflex} onchange={(event) => toggleReflex(event.currentTarget.checked)} />
                    <span class="lever-text">
                      <strong>{LAYA_REFLEX_META.label}</strong>
                      <span class="micro">{LAYA_REFLEX_META.explanation}</span>
                      {#if LAYA_REFLEX_META.note}<span class="micro flag-note">{LAYA_REFLEX_META.note}</span>{/if}
                    </span>
                  </label>
                </div>
                <div class="section-actions">
                  {#if gatesError}<p class="section-error" role="alert">{gatesError}</p>{/if}
                  {#if gatesSaved && !gatesDirty}<p class="section-note">Applies to new turns.</p>{/if}
                  <div class="section-buttons">
                    <button class="button button-quiet" disabled={gatesSaving || !gatesDirty} onclick={discardGates}>Discard</button>
                    <button class="button button-primary" disabled={gatesSaving || !gatesDirty} onclick={() => void saveGates()}>
                      {gatesSaving ? "Saving…" : "Save Mnemo1 gates"}
                    </button>
                  </div>
                </div>
              </section>
            {/if}
          {/if}
        </div>
      </details>
    {/if}

    <!-- (e) The Laya section -->
    <section class="group" aria-label="Laya">
      <header>
        <h3>Laya</h3>
        <p class="micro">
          A small local classifier. With it off nothing changes — every fall-back keeps today's behavior byte-for-byte. The tier-picking settings live at the top of this page.
        </p>
      </header>
      {#if snapshot !== null && topControl !== null}
        <div class="status-row">
          <span class="status-state">Status: {LAYA_STATUS_META[shownStatus]}</span>
          {#if !snapshot.enabled && topControl.laya_status !== "not_installed"}
            <button class="button button-primary" disabled={setupBusy} onclick={startSetup}>{setupBusy ? "Turning on…" : "Turn on"}</button>
          {:else if topControl.laya_status === "not_installed"}
            <button class="button button-quiet" disabled={setupBusy} onclick={startSetup}>{setupBusy ? "Installing…" : "Install"}</button>
          {/if}
          {#if snapshot.enabled}
            <button class="button button-quiet" onclick={stopLaya}>Turn off</button>
          {/if}
        </div>
        <p class="micro">{layaStatusHint(shownStatus)}</p>
        {#if topControl.laya_status === "ready" && topControl.checkpoint !== null}
          <p class="micro">Checkpoint: {topControl.checkpoint}</p>
        {/if}
        {#if flagsDraft !== null}
          <div class="lever-list">
            {#each LAYA_FLAG_META as flag (flag.key)}
              <label class="lever-row">
                <input type="checkbox" bind:checked={flagsDraft[flag.key]} />
                <span class="lever-text">
                  <strong>{flag.label}</strong>
                  <span class="micro">{flag.explanation}</span>
                  {#if flag.note}<span class="micro flag-note">{flag.note}</span>{/if}
                </span>
              </label>
            {/each}
          </div>
          <div class="section-actions">
            {#if flagsError}<p class="section-error" role="alert">{flagsError}</p>{/if}
            {#if flagsSaved && !flagsDirty}<p class="section-note">Applies to new turns.</p>{/if}
            <div class="section-buttons">
              <button
                class="button button-quiet"
                disabled={flagsSaving || !flagsDirty}
                onclick={() => {
                  if (snapshot === null) return;
                  flagsDraft = { ...snapshot.flags };
                  flagsError = "";
                  flagsSaved = false;
                }}
              >
                Discard
              </button>
              <button
                class="button button-primary"
                disabled={flagsSaving || !flagsDirty}
                onclick={() => flagsDraft !== null && void saveFlags($state.snapshot(flagsDraft) as LayaFlags)}
              >
                {flagsSaving ? "Saving…" : "Save flags"}
              </button>
            </div>
          </div>
        {/if}
      {:else}
        <p class="micro">Loading…</p>
      {/if}
    </section>
  </div>
</div>

<style>
  .tiers-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .tiers-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-body); font-weight: 650; }
  .group h4 { margin: 0; font-size: var(--text-body); font-weight: 650; }
  .micro { color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }
  .nested { border-style: dashed; }

  /* The top control */
  .choice-grid { display: grid; gap: 8px; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); }
  .choice-card { display: grid; gap: 4px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); cursor: pointer; align-content: start; }
  .choice-card.selected { border-color: var(--accent); box-shadow: var(--shadow-chip); }
  .choice-card input { accent-color: var(--accent); }
  .choice-card:has(input:disabled) { opacity: 0.55; cursor: not-allowed; }
  .choice-title { font-weight: 650; font-size: var(--text-body); }
  .readiness { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
  .status-state { font-weight: 650; font-size: var(--text-body); }
  .status-state.ok { color: var(--accent); }
  .failure-note { overflow-wrap: anywhere; }
  .progress { height: 6px; border-radius: 4px; background: var(--surface); overflow: hidden; }
  .progress > div { height: 100%; background: var(--accent); transition: width 0.3s; }

  /* The ladder */
  .tier-card { display: grid; gap: 8px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
  .tier-head { display: flex; align-items: center; gap: 10px; justify-content: space-between; }
  .tier-name { display: grid; gap: 2px; flex: 1; max-width: 320px; }
  .tier-name input { padding: 5px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--canvas); color: var(--ink); font-size: var(--text-body); }
  .laya-label { padding: 3px 9px; border-radius: 999px; border: 1px solid var(--accent); color: var(--accent); font-size: var(--text-meta); font-weight: 650; white-space: nowrap; }
  .tier-definition { display: grid; gap: 2px; }
  .tier-definition textarea { padding: 5px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--canvas); color: var(--ink); font-size: var(--text-body); resize: vertical; }
  .model-list { display: grid; gap: 4px; }
  .model-row { display: flex; align-items: center; gap: 10px; }
  .model-row code { font-size: var(--text-meta); overflow-wrap: anywhere; }
  .row-buttons { display: flex; gap: 4px; margin-left: auto; }
  .icon-button { display: inline-flex; padding: 3px; border: 0; border-radius: 6px; background: transparent; color: var(--muted); cursor: pointer; }
  .icon-button:hover:not(:disabled) { color: var(--ink); background: var(--canvas); }
  .icon-button:disabled { opacity: 0.35; cursor: not-allowed; }
  .model-list select { padding: 5px 8px; border: 1px dashed var(--line); border-radius: 8px; background: var(--canvas); color: var(--muted); font-size: var(--text-body); }
  .tier-foot { display: flex; align-items: end; gap: 14px; flex-wrap: wrap; }
  .tier-effort { display: grid; gap: 2px; }
  .tier-effort input { padding: 5px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--canvas); color: var(--ink); font-size: var(--text-body); }
  .hint { margin: 0; padding-bottom: 6px; }

  /* The dispatch text preview */
  .preview { margin: 0; padding: 10px 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--canvas); font-size: var(--text-meta); white-space: pre-wrap; overflow-wrap: anywhere; }

  /* Routing details */
  .routing-details { border: 1px dashed var(--line); border-radius: 14px; background: var(--canvas); padding: 10px 18px; }
  .routing-details > summary { cursor: pointer; color: var(--muted); font-weight: 650; font-size: var(--text-body); }
  .details-body { display: grid; gap: 14px; padding: 12px 0 2px; }

  .knob-list { display: grid; gap: 8px; }
  .knob-row { display: grid; grid-template-columns: minmax(140px, 200px) 1fr minmax(90px, 120px); gap: 8px; align-items: center; }
  .knob-row label { font-weight: 600; font-size: var(--text-body); }
  .knob-row input, .knob-row select { padding: 5px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--canvas); color: var(--ink); font-size: var(--text-body); }
  .lever-list { display: grid; gap: 8px; }
  .lever-row { display: flex; gap: 10px; align-items: start; }
  .lever-row input { margin-top: 3px; accent-color: var(--accent); }
  .lever-text { display: grid; gap: 2px; }
  .flag-note { font-style: italic; }
  .summary-list { display: grid; gap: 4px; margin: 4px 0 0; }
  .summary-list > div { display: flex; gap: 8px; justify-content: space-between; }
  .summary-list dt { color: var(--muted); font-size: var(--text-meta); }
  .summary-list dd { margin: 0; font-size: var(--text-meta); overflow-wrap: anywhere; }
  .section-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .section-buttons { display: flex; gap: 8px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-meta); overflow-wrap: anywhere; }
  .section-note { margin: 0; color: var(--accent); font-size: var(--text-meta); }
  .eval-block { display: grid; gap: 8px; border-top: 1px dashed var(--line); padding-top: 10px; }
  .eval-report { display: grid; gap: 6px; }
  .eval-report h4 { margin: 0; font-size: var(--text-meta); }
  .eval-report table { border-collapse: collapse; font-size: var(--text-meta); }
  .eval-report th, .eval-report td { padding: 2px 10px 2px 0; text-align: left; }
  .miss-list { margin: 0; padding-left: 16px; }
  .status-row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
</style>
