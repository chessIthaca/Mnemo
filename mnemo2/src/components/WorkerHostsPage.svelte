<script lang="ts">
  import {
    WORKER_HOSTS_REFRESH_MS,
    checksLabel,
    hostStateLabel,
    osLabel,
    type PairOffer,
    type WorkerHostsList,
    type WorkerHostsTransport,
  } from "../lib/workerHosts";
  import { errorText } from "../lib/settingsState";

  /**
   * The Worker hosts page: the other Windows or Mac computers that run Mnemo
   * workers. It lists them (refreshed every few seconds), pairs a new one with
   * a short-lived code, links a host to a saved SSH connection (the lead's
   * standing approval to start Claude Code workers there) and revokes a host
   * after a second click. The lead's `team_hosts` tool runs the same code.
   */
  let { transport }: { transport: WorkerHostsTransport } = $props();

  let list = $state<WorkerHostsList | null>(null);
  let listError = $state("");
  let now = $state(Date.now());
  /** True while a timer-driven list call runs; ticks skip meanwhile. */
  let listing = false;

  let busy = $state<Record<string, boolean>>({});
  let errors = $state<Record<string, string>>({});
  let sshInput = $state<Record<string, string>>({});
  let confirmRevoke = $state<string | null>(null);

  let pairName = $state("");
  let offer = $state<PairOffer | null>(null);
  let remaining = $state(0);
  let copied = $state<number | null>(null);

  const noLead = $derived(listError.includes("no open lead"));

  async function refresh(force: boolean): Promise<void> {
    if (!force && listing) return;
    listing = true;
    try {
      list = await transport.list();
      listError = "";
      now = Date.now();
    } catch (error) {
      listError = errorText(error);
    } finally {
      listing = false;
    }
  }

  $effect(() => {
    void refresh(false);
    const id = setInterval(() => void refresh(false), WORKER_HOSTS_REFRESH_MS);
    return () => clearInterval(id);
  });

  // The pairing countdown ticks once a second while a code is showing.
  $effect(() => {
    if (!offer) return;
    const id = setInterval(() => {
      if (remaining > 0) remaining -= 1;
    }, 1000);
    return () => clearInterval(id);
  });

  /** Run one action with its own busy flag and inline error, then re-read the list. */
  async function act(key: string, call: () => Promise<unknown>): Promise<boolean> {
    busy[key] = true;
    errors[key] = "";
    try {
      await call();
      await refresh(true);
      return true;
    } catch (error) {
      errors[key] = errorText(error);
      return false;
    } finally {
      busy[key] = false;
    }
  }

  function validSsh(name: string): boolean {
    const length = (sshInput[name] ?? "").trim().length;
    return length >= 1 && length <= 100;
  }

  async function link(name: string): Promise<void> {
    const ssh = (sshInput[name] ?? "").trim();
    if (await act(`link:${name}`, () => transport.link(name, ssh))) sshInput[name] = "";
  }

  async function unlink(name: string): Promise<void> {
    await act(`unlink:${name}`, () => transport.unlink(name));
  }

  async function revoke(name: string): Promise<void> {
    if (await act(`revoke:${name}`, () => transport.revoke(name))) confirmRevoke = null;
  }

  async function getCode(): Promise<void> {
    busy.pair = true;
    errors.pair = "";
    try {
      const name = pairName.trim();
      const made = await transport.pair(name === "" ? undefined : name);
      offer = made;
      remaining = made.expires_in_s;
      copied = null;
    } catch (error) {
      errors.pair = errorText(error);
    } finally {
      busy.pair = false;
    }
  }

  function copy(index: number, text: string): void {
    navigator.clipboard
      .writeText(text)
      .then(() => {
        copied = index;
        setTimeout(() => {
          if (copied === index) copied = null;
        }, 2000);
      })
      .catch(() => {});
  }

  function clock(seconds: number): string {
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  }
</script>

<section class="hosts-page" aria-label="Worker hosts">
  <div class="hosts-scroll">
    <header>
      <h3>Worker hosts</h3>
      <p class="micro">
        Other Windows or Mac computers that run Mnemo workers for your projects. Mnemo places workers on them by itself.
      </p>
    </header>

    {#if noLead}
      <p class="micro">Open a project to manage worker hosts.</p>
    {:else}
      {#if listError}
        <p class="status-line" role="status">{listError}</p>
      {/if}
      {#if list && list.hosts.length === 0}
        <p class="micro">No worker hosts yet.</p>
      {/if}
      {#if list}
        <ul class="host-list">
          {#each list.hosts as host (host.node_id)}
            <li class="host">
              <div class="host-head">
                <strong>{host.name}</strong>
                <span class="meta">{osLabel(host.os)}</span>
                <span class="meta state"><span class="dot" class:on={host.connected}></span>{hostStateLabel(host, now)}</span>
                {#if host.busy !== null && host.slots !== null}
                  <span class="meta">{host.busy}/{host.slots} busy</span>
                {/if}
                <span class="meta tone-{checksLabel(host.checks).tone}">{checksLabel(host.checks).text}</span>
                {#if host.claude_home !== null}
                  <span class="meta">Claude Code: {host.claude_home ? "signed in folder found" : "signed in folder not found"}</span>
                {/if}
              </div>

              {#if host.ssh}
                <div class="row-line">
                  <span>Linked to {host.ssh}</span>
                  <button class="button" disabled={busy[`unlink:${host.name}`]} onclick={() => unlink(host.name)}>Unlink</button>
                </div>
                {#if errors[`unlink:${host.name}`]}
                  <p class="inline-error" role="alert">{errors[`unlink:${host.name}`]}</p>
                {/if}
              {:else}
                <div class="row-line">
                  <input
                    type="text"
                    aria-label="SSH connection name"
                    placeholder="SSH connection name"
                    maxlength="100"
                    bind:value={sshInput[host.name]}
                  />
                  <button class="button" disabled={busy[`link:${host.name}`] || !validSsh(host.name)} onclick={() => link(host.name)}>Link</button>
                </div>
                <p class="hint">Linking lets the lead start Claude Code workers on this host over that SSH connection. Unlink to withdraw.</p>
                {#if errors[`link:${host.name}`]}
                  <p class="inline-error" role="alert">{errors[`link:${host.name}`]}</p>
                {/if}
              {/if}

              {#if confirmRevoke === host.name}
                <div class="delete-confirm" role="alert">
                  <p>Revoke {host.name}? Its running workers stop and can be resumed with team_resume.</p>
                  <div class="row">
                    <button class="button" onclick={() => (confirmRevoke = null)}>Cancel</button>
                    <button class="button button-danger" disabled={busy[`revoke:${host.name}`]} onclick={() => revoke(host.name)}>Confirm revoke</button>
                  </div>
                </div>
              {:else}
                <div class="row">
                  <button class="button" onclick={() => (confirmRevoke = host.name)}>Revoke</button>
                </div>
              {/if}
              {#if errors[`revoke:${host.name}`]}
                <p class="inline-error" role="alert">{errors[`revoke:${host.name}`]}</p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      <section class="pair" aria-label="Pair a host">
        <h4>Pair a host</h4>
        {#if offer}
          <div class="offer">
            <p class="code">{offer.code}</p>
            {#if remaining > 0}
              <p class="micro">Expires in <span class="countdown">{clock(remaining)}</span></p>
            {:else}
              <p class="micro">expired</p>
              <button class="button" disabled={busy.pair} onclick={getCode}>Get a new code</button>
            {/if}
            {#each offer.commands as command, index (index)}
              <div class="command">
                <code>{command}</code>
                <button class="button" onclick={() => copy(index, command)}>{copied === index ? "Copied" : "Copy"}</button>
              </div>
            {/each}
            <p class="micro">{offer.note}</p>
            <p class="micro">Install mnemo2-node on the other computer first; see the Worker hosts guide in the docs.</p>
            <div class="row">
              <button class="button" onclick={() => (offer = null)}>Done</button>
            </div>
          </div>
        {:else}
          <div class="row-line">
            <input type="text" aria-label="Host name (optional)" placeholder="Name (optional)" bind:value={pairName} />
            <button class="button button-primary" disabled={busy.pair} onclick={getCode}>Get pairing code</button>
          </div>
        {/if}
        {#if errors.pair}
          <p class="inline-error" role="alert">{errors.pair}</p>
        {/if}
      </section>
    {/if}
  </div>
</section>

<style>
  .hosts-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .hosts-scroll { display: grid; flex: 1; align-content: start; gap: 14px; padding: 18px 24px; overflow-y: auto; }
  header { display: grid; gap: 4px; }
  h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  h4 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .hint { margin: 0; color: var(--muted); font-size: var(--text-meta); }
  .status-line { margin: 0; color: var(--danger); font-size: var(--text-meta); overflow-wrap: anywhere; }
  .inline-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }

  .host-list { display: grid; gap: 10px; margin: 0; padding: 0; list-style: none; }
  .host, .pair { display: grid; gap: 8px; padding: 14px 16px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .host-head { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px; font-size: var(--text-body); }
  .meta { color: var(--muted); font-size: var(--text-meta); }
  .state { display: inline-flex; align-items: center; gap: 5px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); }
  .dot.on { background: var(--accent); }
  .tone-ok { color: var(--accent); }
  .tone-bad { color: var(--danger); }
  .row-line { display: flex; align-items: center; gap: 8px; font-size: var(--text-body); }
  .row-line input { flex: 1; min-width: 0; }
  .row { display: flex; justify-content: flex-end; gap: 8px; }

  .offer { display: grid; gap: 8px; }
  .code { margin: 0; font-family: var(--mono, monospace); font-size: 1.8rem; font-weight: 650; letter-spacing: 0.12em; }
  .countdown { font-variant-numeric: tabular-nums; }
  .command { display: flex; align-items: center; gap: 8px; }
  .command code { flex: 1; min-width: 0; padding: 7px 9px; border-radius: 6px; background: var(--surface-soft); font-size: 0.86em; overflow-wrap: anywhere; }

  .delete-confirm { display: grid; gap: 6px; padding: 10px 12px; border: 1px solid var(--danger-tint-line); border-radius: 12px; background: var(--danger-tint); font-size: var(--text-body); }
  .delete-confirm p { margin: 0; color: var(--muted); }
  .button-danger { background: var(--danger); color: var(--on-danger); }
</style>
