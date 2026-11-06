<script lang="ts">
  import {
    SAFETY_MODES,
    SAFETY_RULE_KINDS,
    emptyRule,
    ruleDraft,
    rulesDiffer,
    safetyModeMeta,
    type SafetyModeId,
    type SafetyRule,
    type SafetySettings,
    type SafetyTransport,
  } from "../lib/safetySettings";
  import { errorText } from "../lib/settingsState";
  import { trackUnsaved } from "../lib/unsavedEdits";

  /**
   * The Safety settings page: the four `[general] safety` approval modes
   * (the autonomous one visibly marked risky) and the open project's
   * auto-approve rules. Both save on their own button through the
   * sanctioned settings pipeline — the mode moves the open lead's live
   * approval gate on its next action, and the rules replace
   * `<project>/.coding/safety.toml`, which the lead re-reads on its next
   * dispatch. Workers never prompt in any mode, and the page says so
   * instead of implying a gate the team does not have.
   */
  let { transport }: { transport: SafetyTransport } = $props();

  /** The snapshot; null until the first read lands. */
  let snapshot = $state<SafetySettings | null>(null);
  let loadError = $state("");

  let modeDraft = $state<SafetyModeId>("approve-each-action");
  let modeSaving = $state(false);
  let modeError = $state("");
  let modeSaved = $state(false);

  let rulesDraft = $state<SafetyRule[]>([]);
  let rulesOriginal = $state<SafetyRule[]>([]);
  let rulesSaving = $state(false);
  let rulesError = $state("");
  let rulesSaved = $state(false);

  const modeDirty = $derived(modeDraft !== (snapshot?.mode ?? modeDraft));
  const rulesDirty = $derived(rulesDiffer(rulesDraft, rulesOriginal));
  trackUnsaved(() => modeDirty || rulesDirty);
  /** The draft selects the no-prompt mode: show the standing warning. */
  const riskySelected = $derived(safetyModeMeta(modeDraft).risky);

  $effect(() => {
    let cancelled = false;
    loadError = "";
    transport
      .get()
      .then((settings) => {
        if (!cancelled) accept(settings);
      })
      .catch((failure) => {
        if (!cancelled) loadError = errorText(failure);
      });
    return () => {
      cancelled = true;
    };
  });

  /** Adopt a snapshot; a dirty draft keeps its unsaved edits. */
  function accept(settings: SafetySettings): void {
    snapshot = settings;
    loadError = "";
    if (!modeDirty) modeDraft = settings.mode;
    if (!rulesDirty) {
      rulesDraft = settings.rules.map((rule) => ({ ...rule }));
      rulesOriginal = settings.rules.map((rule) => ({ ...rule }));
    }
  }

  async function saveMode(): Promise<void> {
    modeError = "";
    modeSaved = false;
    modeSaving = true;
    try {
      accept(await transport.saveMode(modeDraft));
      modeSaved = true;
    } catch (failure) {
      modeError = errorText(failure);
    } finally {
      modeSaving = false;
    }
  }

  function discardMode(): void {
    modeDraft = snapshot?.mode ?? modeDraft;
    modeError = "";
    modeSaved = false;
  }

  async function saveRules(): Promise<void> {
    rulesError = "";
    rulesSaved = false;
    const checked: SafetyRule[] = [];
    for (const rule of rulesDraft) {
      const parsed = ruleDraft(rule);
      if ("error" in parsed) {
        rulesError = parsed.error;
        return;
      }
      checked.push(parsed.rule);
    }
    rulesSaving = true;
    try {
      accept(await transport.saveRules(checked));
      rulesSaved = true;
    } catch (failure) {
      rulesError = errorText(failure);
    } finally {
      rulesSaving = false;
    }
  }

  function discardRules(): void {
    rulesDraft = rulesOriginal.map((rule) => ({ ...rule }));
    rulesError = "";
    rulesSaved = false;
  }

  function addRule(): void {
    rulesDraft = [...rulesDraft, emptyRule()];
    rulesSaved = false;
  }

  function removeRule(index: number): void {
    rulesDraft = rulesDraft.filter((_, position) => position !== index);
    rulesSaved = false;
  }
</script>

<div class="safety-page">
  <div class="safety-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if snapshot === null}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="Approval mode">
        <header>
          <h3>Approval mode</h3>
          <p class="micro">
            How much the lead may do without asking you first. Applies to the lead; team workers always run
            without prompts.
          </p>
        </header>
        <div class="mode-list" role="radiogroup" aria-label="Approval mode">
          {#each SAFETY_MODES as mode (mode.id)}
            <label class="mode-row" class:risky={mode.risky} class:selected={modeDraft === mode.id}>
              <input type="radio" name="safety-mode" value={mode.id} bind:group={modeDraft} />
              <span class="mode-text">
                <span class="mode-title">
                  <strong>{mode.label}</strong>
                  {#if mode.risky}<span class="risky-badge">Risky</span>{/if}
                  {#if snapshot.mode === mode.id}<span class="current-badge">Current</span>{/if}
                </span>
                <span class="micro">{mode.explanation}</span>
              </span>
            </label>
          {/each}
        </div>
        {#if riskySelected}
          <p class="risky-note" role="note">
            Run without asking removes every approval prompt for the lead — file writes, shell commands and the
            lead's own merges and pushes run unchecked.
          </p>
        {/if}
        <div class="section-actions">
          {#if modeError}<p class="section-error" role="alert">{modeError}</p>{/if}
          {#if modeSaved && !modeDirty}
            <p class="section-note">
              {#if snapshot.live_lead}Saved. The lead's next action follows it — no restart.{:else}Saved. Applies the next time a project opens.{/if}
            </p>
          {/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={modeSaving || !modeDirty} onclick={discardMode}>Discard</button>
            <button class="button button-primary" disabled={modeSaving || !modeDirty} onclick={saveMode}>
              {modeSaving ? "Saving…" : "Save mode"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Safety rules">
        <header>
          <h3>Safety rules</h3>
          <p class="micro">
            Auto-approve rules for this project: a tool call that matches one runs without asking, even in an
            asking mode.
          </p>
        </header>
        {#if !snapshot.project_open}
          <p class="micro">
            Open a project to edit its rules — they live in the project's <code>.coding/safety.toml</code>.
          </p>
        {:else}
          {#if rulesDraft.length === 0}
            <p class="micro">No rules yet — every action the mode gates still asks.</p>
          {:else}
            <div class="rule-list">
              {#each rulesDraft as rule, index (index)}
                <div class="rule-row">
                  <input
                    class="rule-tool"
                    bind:value={rule.tool}
                    placeholder="tool"
                    aria-label={`Rule ${index + 1} tool`}
                    spellcheck="false"
                  />
                  <select class="rule-kind" bind:value={rule.kind} aria-label={`Rule ${index + 1} kind`}>
                    {#each SAFETY_RULE_KINDS as kind (kind.value)}
                      <option value={kind.value}>{kind.label}</option>
                    {/each}
                  </select>
                  <input
                    class="rule-pattern"
                    bind:value={rule.pattern}
                    placeholder="pattern"
                    aria-label={`Rule ${index + 1} pattern`}
                    spellcheck="false"
                  />
                  <button
                    class="button button-quiet rule-remove"
                    aria-label={`Remove rule ${index + 1}`}
                    onclick={() => removeRule(index)}
                  >
                    Remove
                  </button>
                </div>
              {/each}
            </div>
          {/if}
          <p class="micro">
            {#each SAFETY_RULE_KINDS as kind, position (kind.value)}{position > 0 ? " " : ""}<strong>{kind.label}:</strong>
              {kind.blurb}{/each}
          </p>
          <div class="rule-add">
            <button class="button button-quiet" onclick={addRule}>Add rule</button>
          </div>
          <div class="section-actions">
            {#if rulesError}<p class="section-error" role="alert">{rulesError}</p>{/if}
            {#if rulesSaved && !rulesDirty}<p class="section-note">Saved. The lead follows them on its next action.</p>{/if}
            <div class="section-buttons">
              <button class="button button-quiet" disabled={rulesSaving || !rulesDirty} onclick={discardRules}>Discard</button>
              <button class="button button-primary" disabled={rulesSaving || !rulesDirty} onclick={saveRules}>
                {rulesSaving ? "Saving…" : "Save rules"}
              </button>
            </div>
          </div>
        {/if}
      </section>

      <section class="group" aria-label="When this applies">
        <header>
          <h3>When this applies</h3>
        </header>
        <p class="micro">
          {#if snapshot.live_lead}
            Changes apply to the lead's very next action in the open project — no restart.
          {:else}
            Changes apply the next time a project opens.
          {/if}
          Team workers are not gated by the mode or the rules: they run autonomously and each action they take that
          would have asked is recorded as a bypassed approval.
        </p>
      </section>
    {/if}
  </div>
</div>

<style>
  .safety-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .safety-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .micro code { padding: 1px 4px; border-radius: 5px; background: var(--surface-soft); font-size: 0.92em; }

  .mode-list { display: grid; gap: 4px; }
  .mode-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    cursor: pointer;
    font-size: var(--text-body);
  }
  .mode-row.selected { border-color: var(--accent); background: var(--accent-soft); }
  .mode-row.risky.selected { border-color: var(--danger); background: var(--surface); }
  .mode-text { display: grid; gap: 1px; }
  .mode-title { display: flex; align-items: baseline; gap: 8px; }
  .mode-title strong { font-weight: 620; }
  .risky-badge {
    padding: 1px 7px;
    border: 1px solid var(--danger);
    border-radius: 999px;
    color: var(--danger);
    font-size: 0.78em;
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .current-badge { color: var(--muted); font-size: 0.82em; }
  .risky-note {
    margin: 0;
    padding: 8px 10px;
    border-left: 3px solid var(--danger);
    border-radius: 6px;
    background: var(--surface);
    color: var(--danger);
    font-size: var(--text-body);
    line-height: 1.5;
  }

  .rule-list { display: grid; gap: 4px; }
  .rule-row { display: grid; grid-template-columns: minmax(90px, 1fr) minmax(110px, auto) minmax(140px, 2fr) auto; gap: 8px; align-items: center; }
  .rule-row input,
  .rule-row select {
    min-width: 0;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-size: var(--text-body);
  }
  .rule-remove { min-height: 32px; padding: 5px 10px; }
  .rule-add { display: flex; justify-content: flex-start; }
  .rule-add .button { min-height: 32px; padding: 5px 10px; }

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; flex-shrink: 0; }

  @media (max-width: 720px) {
    .rule-row { grid-template-columns: 1fr 1fr; }
    .rule-pattern { grid-column: 1 / -1; }
  }
</style>
