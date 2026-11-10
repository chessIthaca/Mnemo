<script lang="ts">
  import { ABOUT_GROUPS, APP_LICENSE_URL, REPOSITORY_URL, type AboutTransport } from "../lib/about";

  /**
   * The About page: the running version, Mnemo's license and source code, and
   * the open-source projects Mnemo2 is built on — every entry credited with
   * its license and a link to both.
   *
   * The version arrives from the desktop shell once the page mounts; a failed
   * read simply leaves the version chip out — the page never shows an error.
   * Links open in the system browser through the shell transport; outside the
   * desktop app the page opens them itself.
   */
  let { transport }: { transport: AboutTransport | null } = $props();

  /** The running app's version; empty until the shell answers, and then hidden. */
  let version = $state("");

  $effect(() => {
    const active = transport;
    if (!active) return;
    let cancelled = false;
    active.version().then(
      (loaded) => {
        if (!cancelled) version = loaded;
      },
      () => {
        // Leave the chip out; everything else on the page still works.
      },
    );
    return () => {
      cancelled = true;
    };
  });

  /** Open a link in the system browser (the page opens it outside the desktop app). */
  function open(url: string): void {
    if (transport) void transport.open(url);
    else window.open(url, "_blank", "noopener,noreferrer");
  }
</script>

<div class="about-page">
  <div class="about-scroll">
    <header class="heading">
      <h3>Mnemo2</h3>
      {#if version}
        <span class="version-chip">v{version}</span>
      {/if}
    </header>

    <p class="intro">
      Mnemo2 is the team cockpit of Mnemo. It is free software under the
      <button class="link-button" type="button" onclick={() => open(APP_LICENSE_URL)}>MIT License</button>.
    </p>
    <button class="link-button" type="button" onclick={() => open(REPOSITORY_URL)}>Source code</button>

    <p class="intro">Built on these open-source projects:</p>

    {#each ABOUT_GROUPS as group (group.title)}
      <section class="group" aria-label={group.title}>
        <h4>{group.title}</h4>
        <ul>
          {#each group.entries as entry (entry.name)}
            <li>
              <button class="link-button package" type="button" title={`Open ${entry.name}`} onclick={() => open(entry.registry)}>{entry.name}</button>
              <button
                class="license-chip"
                type="button"
                title={`Show the ${entry.license} license`}
                onclick={() => open(entry.licenseUrl)}
              >{entry.license}</button>
            </li>
          {/each}
        </ul>
      </section>
    {/each}
  </div>
</div>

<style>
  .about-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .about-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; overflow-x: hidden; }

  .heading { display: flex; align-items: center; gap: 10px; }
  .heading h3 { margin: 0; font-size: var(--text-page); font-weight: 650; }
  .version-chip {
    padding: 3px 8px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink-soft);
    font-size: var(--text-meta);
    font-weight: 620;
  }

  .intro { margin: 0; color: var(--ink); font-size: var(--text-body); line-height: 1.55; }
  .link-button { padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: var(--text-body); font-weight: 600; text-align: left; }
  .link-button:hover { text-decoration: underline; }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group h4 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .group ul { display: grid; gap: 2px; margin: 0; padding: 0; list-style: none; }
  .group li { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .package { min-width: 0; overflow-wrap: anywhere; }
  .license-chip {
    flex-shrink: 0;
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink-soft);
    font-size: var(--text-meta);
    font-weight: 620;
    cursor: pointer;
  }
  .license-chip:hover { color: var(--accent-strong); }
</style>
