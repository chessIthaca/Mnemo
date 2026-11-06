<script lang="ts">
  import { ArrowDown, ArrowUp, X } from "@lucide/svelte";
  import { untrack } from "svelte";
  import type { IndexPass } from "../lib/indexingEngine";
  import type { EndpointDetailDto, SettingsTransport, TiersDto } from "../lib/settingsEngine";
  import { allSpecs, cloneTiers, errorText, LAYA_LABELS, moveItem, priceOfSpec } from "../lib/settingsState";
  import {
    checkSummaryText,
    LAYA_FLAG_META,
    LAYA_STATUS_META,
    lowCoverage,
    modeAvailable,
    modeUnlockText,
    shareText,
    type LayaEvent,
    type LayaFlagKey,
    type LayaStatus,
    type LayaTransport,
    type RoutingStatusWire,
    type TierChoice,
    type TiersTopControl,
  } from "../lib/layaSettings";
  import { trackUnsaved } from "../lib/unsavedEdits";

  /**
   * The Settings > Tiers page (plan 8106337a, design §7): one choice — does
   * Laya help pick the tier — with the self-running Laya check and its
   * progress under it, the five-rung ladder editor, and three collapsed
   * sections: what the lead sees, how Laya is doing, and the other things
   * Laya can do. Every routing knob the old page exposed is applied by the
   * check itself; the only thing left to save by hand is the ladder.
   */
  let {
    transport,
    layaTransport,
    endpoints,
    tiers,
    ontiers,
    evalPass = null,
  }: {
    transport: SettingsTransport | null;
    layaTransport: LayaTransport | null;
    endpoints: EndpointDetailDto[];
    tiers: TiersDto;
    ontiers: (next: TiersDto) => void;
    /** The Laya check's pass from the store's indexing snapshot. */
    evalPass: IndexPass | null;
  } = $props();

  // ── §7.1 The top control ────────────────────────────────────────────────

  /** The control's own wire: the saved choice plus Laya's readiness. */
  let topControl = $state<TiersTopControl | null>(null);
  /** The full Laya snapshot — the runtime state, the flags and Mnemo1's gates. */
  let snapshot = $state<LayaStatus | null>(null);
  /** The Mnemo2 `[routing]` table, its log summary and the last check's record. */
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
   * check and how-Laya-is-doing sections follow it). */
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

  /** Whether a choice is past the labelled decisions it unlocks at — below
   * that the card shows a hint instead of refusing the pick. */
  function choiceUnlocked(target: TierChoice): boolean {
    if (target === "off" || target === "shadow") return true;
    if (routingStatus === null) return false;
    return modeAvailable(target, routingStatus.summary);
  }

  /** Save a choice through the one combined command (design §7.1). */
  async function saveChoice(target: TierChoice): Promise<void> {
    if (layaTransport === null) return;
    savingChoice = true;
    setupError = "";
    try {
      topControl = await layaTransport.saveTopControl(target);
      // The mode is half of the `[routing]` table — re-read it so the
      // summary and the check's record follow, and refresh the status
      // behind it.
      await refreshRouting();
      await refreshAll();
    } catch (error) {
      setupError = errorText(error);
    } finally {
      savingChoice = false;
    }
  }

  /** Pick a radio card: one click, one save. */
  async function pickChoice(target: TierChoice): Promise<void> {
    if (choiceDisabled(target) || target === choice) return;
    await saveChoice(target);
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

  /** Re-read the `[routing]` table + its summary (after a check or a save). */
  async function refreshRouting(): Promise<void> {
    const active = layaTransport;
    if (active === null) return;
    try {
      routingStatus = await active.routingStatus();
    } catch (error) {
      loadError = errorText(error);
    }
  }

  // ── The Laya check ─────────────────────────────────────────────────────

  /** Whether the check pass is running right now. */
  const checkRunning = $derived(evalPass?.state === "running");

  /** What went wrong starting a check from the Check again button. */
  let checkError = $state("");

  /** Run the check on demand; the pass itself streams through the store. */
  async function checkAgain(): Promise<void> {
    if (layaTransport === null) return;
    checkError = "";
    try {
      await layaTransport.checkLaya();
    } catch (error) {
      checkError = errorText(error);
    }
  }

  /** Re-read the routing table whenever a check finishes, so the page shows
   * the record and the gates the check just applied. */
  let lastPassState: string | null = null;
  $effect(() => {
    const state = evalPass?.state ?? null;
    if (state === "done" && lastPassState !== "done") void refreshRouting();
    lastPassState = state;
  });

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
      // The stored copy is the truth — a save that normalized a rung (say
      // `resolves_to`) must not leave the draft dirty against the new wire.
      draft = cloneTiers(stored);
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

  // ── The other Laya flags — one switch, one save ────────────────────────

  /** The flag with a save in flight; every switch disables while one runs. */
  let flagSaving = $state<LayaFlagKey | null>(null);
  let flagError = $state("");

  /** Flip one opt-in flag: save it, then re-read; revert the box on failure. */
  async function toggleFlag(key: LayaFlagKey, input: HTMLInputElement): Promise<void> {
    if (layaTransport === null) return;
    const next = input.checked;
    flagSaving = key;
    flagError = "";
    try {
      await layaTransport.saveFlag(key, next);
      await refreshAll();
    } catch (error) {
      input.checked = !next;
      flagError = errorText(error);
    } finally {
      flagSaving = null;
    }
  }

  // ── Loading, events and leaving with unsaved edits ─────────────────────

  /** The transport the page loads with — a derived, so prop updates the
   * load does not care about (the check's progress ticks) never re-run it. */
  const activeTransport = $derived(layaTransport);

  /** Load everything once per transport (the dialog keeps it open). */
  $effect(() => {
    const active = activeTransport;
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

  /** Only the ladder's own edits hold the page: everything else saves on
   * its own click or is applied by the check. */
  trackUnsaved(() => ladderChanged);
</script>


<datalist id="effort-options">
  <option value="low"></option>
  <option value="medium"></option>
  <option value="high"></option>
</datalist>

<div class="tiers-page">
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
            {#if entry.choice !== "off" && !choiceUnlocked(entry.choice) && routingStatus !== null}
              <span class="micro">Best after {modeUnlockText(entry.choice, routingStatus.summary)}.</span>
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
            <span class="micro">Learned from {topControl.knn_rows} finished workers (uses them from {topControl.knn_min_rows}).</span>
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
        {#if layaHelps && layaTransport !== null && topControl !== null}
          <div class="check" aria-label="Laya check" role="group">
            {#if checkRunning && evalPass !== null}
              <span class="micro">Checking Laya{evalPass.total > 0 ? ` · ${evalPass.done} of ${evalPass.total} test tasks` : ""}…</span>
              {#if evalPass.total > 0}
                <div
                  class="progress"
                  role="progressbar"
                  aria-label="Laya check progress"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={Math.round((evalPass.done / evalPass.total) * 100)}
                >
                  <div style="width: {Math.min(100, Math.round((evalPass.done / evalPass.total) * 100))}%"></div>
                </div>
              {:else}
                <div
                  class="progress"
                  role="progressbar"
                  aria-label="Laya check progress"
                  aria-valuemin={0}
                  aria-valuemax={100}
                >
                  <div style="width: 30%"></div>
                </div>
              {/if}
            {:else if evalPass !== null && evalPass.state === "failed"}
              <p class="section-error" role="alert">The Laya check failed: {evalPass.detail ?? "unknown error"}</p>
            {:else}
              <span class="micro">{checkSummaryText(routingStatus?.eval ?? null)}</span>
              {#if lowCoverage(routingStatus?.eval ?? null)}
                <span class="micro">Laya leaves most choices to the lead until it has learned from more finished workers.</span>
              {/if}
            {/if}
            <button class="button button-quiet" disabled={checkRunning || shownStatus !== "ready"} onclick={() => void checkAgain()}>
              Check again
            </button>
            {#if checkError}<p class="section-error" role="alert">{checkError}</p>{/if}
          </div>
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
    <details class="quiet-details">
      <summary>What the lead sees</summary>
      <p class="micro">
        What `team_dispatch` tells the lead about the `tier` parameter — generated from the ladder above (the draft, not the stored copy, so edits preview before saving).
      </p>
      <pre class="preview">{dispatchPreview}</pre>
    </details>

    <!-- (d) How Laya is doing — collapsed, and only while Laya helps -->
    {#if layaHelps && routingStatus !== null}
      <details class="quiet-details">
        <summary>How Laya is doing</summary>
        <dl class="summary-list">
          <div><dt>Decisions</dt><dd>{decisionsText(routingStatus.summary.decisions_by_mode)}</dd></div>
          <div><dt>Agreement</dt><dd>{shareText(routingStatus.summary.agreement_rate)}</dd></div>
          <div><dt>Laya accuracy</dt><dd>{shareText(routingStatus.summary.laya_accuracy)}</dd></div>
          <div><dt>Lead accuracy</dt><dd>{shareText(routingStatus.summary.lead_accuracy)}</dd></div>
          <div><dt>Labelled</dt><dd>{routingStatus.summary.labelled}</dd></div>
          <div><dt>Overrides</dt><dd>{summaryText(routingStatus.summary.overrides_by_source)}</dd></div>
          <div><dt>Nudges</dt><dd>{routingStatus.summary.nudges} ({routingStatus.summary.nudges_acted_on} acted on)</dd></div>
          <div>
            <dt>Gates</dt>
            <dd>
              {routingStatus.settings.min_confidence_4.toFixed(2)} / {routingStatus.settings.min_confidence_3.toFixed(2)} / {routingStatus.settings.min_confidence_2.toFixed(2)}
            </dd>
          </div>
        </dl>
        <p class="micro">Set by the Laya check.</p>
      </details>
    {/if}

    <!-- (e) The other things Laya can do — collapsed, one switch one save -->
    {#if snapshot !== null}
      <details class="quiet-details">
        <summary>Other things Laya can do</summary>
        <p class="micro">Most of these work best once Laya has been fine-tuned on your logs. Each switch saves right away and applies to new turns.</p>
        <div class="lever-list">
          {#each LAYA_FLAG_META as flag (flag.key)}
            <label class="lever-row">
              <input
                type="checkbox"
                checked={snapshot.flags[flag.key]}
                disabled={flagSaving !== null}
                onchange={(event) => void toggleFlag(flag.key, event.currentTarget)}
              />
              <span class="lever-text">
                <strong>{flag.label}</strong>
                <span class="micro">{flag.explanation}</span>
              </span>
            </label>
          {/each}
        </div>
        {#if flagError}<p class="section-error" role="alert">{flagError}</p>{/if}
      </details>
    {/if}
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
  .micro { color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }

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

  /* The Laya check */
  .check { display: grid; gap: 6px; }

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

  /* The collapsed quiet sections */
  .quiet-details { border: 1px dashed var(--line); border-radius: 14px; background: var(--canvas); padding: 10px 18px; }
  .quiet-details > summary { cursor: pointer; color: var(--muted); font-weight: 650; font-size: var(--text-body); }
  .quiet-details > :not(summary) { margin-top: 10px; }
  .lever-list { display: grid; gap: 8px; }
  .lever-row { display: flex; gap: 10px; align-items: start; }
  .lever-row input { margin-top: 3px; accent-color: var(--accent); }
  .lever-text { display: grid; gap: 2px; }
  .summary-list { display: grid; gap: 4px; margin: 4px 0 0; }
  .summary-list > div { display: flex; gap: 8px; justify-content: space-between; }
  .summary-list dt { color: var(--muted); font-size: var(--text-meta); }
  .summary-list dd { margin: 0; font-size: var(--text-meta); overflow-wrap: anywhere; }
  .section-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .section-buttons { display: flex; gap: 8px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-meta); overflow-wrap: anywhere; }
  .section-note { margin: 0; color: var(--accent); font-size: var(--text-meta); }
</style>
