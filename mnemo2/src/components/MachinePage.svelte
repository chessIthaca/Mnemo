<script lang="ts">
  import {
    machineDraft,
    machineWire,
    modelsHostLine,
    type MachineDraft,
    type MachineSection,
    type ModelSettings,
    type ModelSettingsTransport,
    type ModelsHostStatus,
  } from "../lib/modelSettings";
  import { errorText } from "../lib/settingsState";
  import { trackUnsaved } from "../lib/unsavedEdits";

  /**
   * The Machine settings page (plan d3336a8a step 3): how many heavy build and
   * test commands may run at once across EVERY Mnemo2 window on this computer,
   * plus read-only status — the free space on the volumes that fill up.
   *
   * The limit saves as the "machine" section (`[general.machine]` in
   * config.toml); every running instance reads it before its next heavy
   * command, so a save applies without a restart.
   */
  let { transport }: { transport: ModelSettingsTransport } = $props();

  let loaded = $state(false);
  let loadError = $state("");
  /** The shared model host's status (plan d3336a8a step 11) — the same
   * line the Models page shows; `null` until the read returns. */
  let host = $state<ModelsHostStatus | null>(null);

  let machine = $state({
    draft: emptyDraft(),
    original: emptyDraft(),
    disks: [] as MachineSection["disks"],
    sccache: { found: false, path: null } as MachineSection["sccache"],
    saving: false,
    error: "",
    saved: false,
  });

  function emptyDraft(): MachineDraft {
    return { heavy_commands: "", min: 1, max: 16 };
  }

  const machineDirty = $derived(JSON.stringify(machine.draft) !== JSON.stringify(machine.original));
  trackUnsaved(() => machineDirty);

  $effect(() => {
    let cancelled = false;
    loadError = "";
    // "Refreshed on open": every get() re-reads the volumes, so opening the
    // page (or this tab) shows current free space.
    transport
      .get()
      .then((settings: ModelSettings) => {
        if (cancelled) return;
        machine.draft = machineDraft(settings.machine);
        machine.original = machineDraft(settings.machine);
        machine.disks = settings.machine.disks;
        machine.sccache = settings.machine.sccache;
        loaded = true;
      })
      .catch((failure) => {
        if (!cancelled) loadError = errorText(failure);
      });
    return () => {
      cancelled = true;
    };
  });

  /** The shared model host line (plan d3336a8a step 11): a separate, cheap
   * read; a failure leaves the "could not read" note. */
  $effect(() => {
    let cancelled = false;
    transport
      .hostStatus()
      .then((status) => {
        if (!cancelled) host = status;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  });

  /** The one read-only line the page shows per volume. */
  function diskLine(disk: MachineSection["disks"][number]): string {
    return `Free disk: ${disk.free_gb} GB on ${disk.volume}`;
  }

  /** The one read-only build-cache line: found names the binary, absent
   * explains how to install it (plan d3336a8a step 7). */
  function sccacheLine(cache: MachineSection["sccache"]): string {
    return cache.found
      ? `Shared Rust build cache: sccache (${cache.path ?? ""})`
      : "Shared Rust build cache: off; install it with cargo install sccache";
  }

  /** What the volume is used for, next to its number. */
  function diskRole(disk: MachineSection["disks"][number]): string {
    return disk.role === "config" ? "Mnemo models and memory" : "this project's builds";
  }

  async function saveLimit(): Promise<void> {
    const parsed = machineWire(machine.draft);
    if (parsed.error) {
      machine.error = parsed.error;
      return;
    }
    machine.saving = true;
    machine.error = "";
    try {
      await transport.save("machine", parsed.value);
      machine.original = { ...machine.draft };
      machine.saved = true;
    } catch (failure) {
      machine.error = errorText(failure);
    } finally {
      machine.saving = false;
    }
  }

  function discardLimit(): void {
    machine.draft = { ...machine.original };
    machine.error = "";
    machine.saved = false;
  }
</script>

<div class="machine-page">
  <div class="machine-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if !loaded}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="Machine limits">
        <header>
          <h3>Builds and tests</h3>
          <p class="micro">
            Heavy commands (cargo build/test/clippy, npm test/build/check) from every Mnemo2 window on this computer share these slots.
          </p>
        </header>
        <div class="limit-row">
          <label for="machine-heavy-commands">Builds and tests at once</label>
          <input
            id="machine-heavy-commands"
            inputmode="numeric"
            bind:value={machine.draft.heavy_commands}
            spellcheck="false"
          />
          <span class="micro">From {machine.draft.min} to {machine.draft.max}. The next heavy command in every window waits for a free slot.</span>
        </div>
        <div class="section-actions">
          {#if machine.error}<p class="section-error" role="alert">{machine.error}</p>{/if}
          {#if machine.saved && !machineDirty}<p class="section-note">Applies to the next heavy command in every window.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={machine.saving || !machineDirty} onclick={discardLimit}>Discard</button>
            <button class="button button-primary" disabled={machine.saving || !machineDirty} onclick={saveLimit}>
              {machine.saving ? "Saving…" : "Save limit"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Build cache">
        <header>
          <h3>Build cache</h3>
          <p class="micro">
            One Rust build cache shared by every window on this computer, so a dependency is compiled once instead of once per window.
          </p>
        </header>
        <p class="cache-line">{sccacheLine(machine.sccache)}</p>
      </section>

      <section class="group" aria-label="Shared model host">
        <header>
          <h3>Shared model host</h3>
          <p class="micro">
            One embedding server per computer, serving the memory and code-search models to every window that needs them.
          </p>
        </header>
        {#if host}
          <p class="cache-line">{modelsHostLine(host)}</p>
        {:else}
          <p class="micro">Could not read the shared model host's status.</p>
        {/if}
      </section>

      <section class="group" aria-label="Disk space">
        <header>
          <h3>Disk space</h3>
          <p class="micro">Where the builds and the models land. Read-only — refreshed each time this page opens.</p>
        </header>
        {#if machine.disks.length === 0}
          <p class="micro">Could not read the free space of this machine's volumes.</p>
        {:else}
          <ul class="disk-list">
            {#each machine.disks as disk (disk.role)}
              <li class="disk-row">
                <span class="disk-line">{diskLine(disk)}</span>
                <span class="micro">{diskRole(disk)}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    {/if}
  </div>
</div>

<style>
  .machine-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .machine-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }

  .group {
    display: grid;
    gap: 12px;
    padding: 16px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--surface);
  }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }

  .limit-row { display: grid; grid-template-columns: auto 120px; gap: 6px 18px; align-items: center; }
  .limit-row label { font-size: var(--text-body); font-weight: 620; }
  .limit-row input {
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-size: var(--text-body);
  }
  .limit-row .micro { grid-column: 1 / -1; }

  .cache-line { margin: 0; font-size: var(--text-body); overflow-wrap: anywhere; }

  .disk-list { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; }
  .disk-row { display: flex; align-items: baseline; gap: 10px; font-size: var(--text-body); }
  .disk-line { font-weight: 620; }

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; flex-shrink: 0; }
</style>
