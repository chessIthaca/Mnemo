# Worker hosts

A worker host is a second Windows or Mac computer that runs Mnemo workers for you. This guide sets one up from scratch.

## 1. What a worker host is

Your main PC stays the hub. It holds your projects, your model endpoints and Mnemo's shared memory. The worker host runs the workers themselves, plus their builds and checks.

- Model calls and shared memory go through the hub. The host needs no API keys and no GitHub login.
- Code moves between the two computers as git bundles. Merged work always lands on the hub.
- Hosts must be Windows or Mac computers.

## 2. Install mnemo2-node on the host

`mnemo2-node` is the small program that turns a computer into a worker host. It is built next to the app and is bundled with the Mnemo2 installer (Windows) and app bundle (Mac), so installing Mnemo2 on the host also installs it. It sits in the same folder as the app's other helper programs (`mnemo2-mcp` and `mnemo-models`): the install folder on Windows, `Mnemo2.app/Contents/MacOS` on a Mac.

Until your installer includes it (it ships from the next Mnemo2 release), build it from source with `cargo build --release -p mnemo-app --bin mnemo2-node`. You only use the command line; the host never needs the Mnemo2 window.

## 3. Pair the host

1. On the hub, open **Settings → Worker hosts** and click **Get pairing code**. Optionally type a name first; otherwise the host gets one from its computer name.
2. The page shows a code, a countdown (mm:ss) and two commands, each with a **Copy** button. The code is valid for 10 minutes and works once.
3. On the host, run the first command:

   ```
   mnemo2-node pair --hub <address> --code <code> [--name <name>]
   ```

   `<address>` is the hub's `host:port`.
4. Then start the node, choosing an empty folder for its workspace:

   ```
   mnemo2-node run --workspace <empty folder> [--slots N]
   ```

   `--slots N` is how many checkouts the host keeps warm per project, from 1 to 8 (default 2).

The first pairing makes the hub start listening on its port. Allow it if a firewall prompt appears. Both computers must be able to reach each other: the same network, or Tailscale or a similar private network. Leave `mnemo2-node run` running; it reconnects by itself.

## 4. What you see

In **Settings → Worker hosts** each host shows:

- its operating system and whether it is connected or away,
- its checks: **passed**, **pending** or **failed** with the reason. Mnemo runs the project's checks on a new host before it places workers there,
- how many of its slots are busy (for example 1/2 busy).

Every worker shows which computer runs it: a host badge (for example `studio`) in the worker list and `host: studio` in the worker's detail. While the host is away and the worker is still active, the badge reads `studio · away …` with the time, and the detail explains it. A finished worker shows no away state.

Mnemo places workers on hosts by itself. To choose one, the lead passes `host` to `team_dispatch`.

## 5. Claude Code workers on a host

Claude Code workers run in the Claude desktop app, over SSH. To allow them on a host:

1. In the Claude desktop app, add an SSH connection to the host. It must log in as the same user that runs `mnemo2-node`.
2. Sign in to Claude Code on the host once.
3. In **Settings → Worker hosts**, type the SSH connection's name next to the host and click **Link**. The host then shows "Linked to <name>".

Linking is your standing approval for the lead to start Claude Code workers on that host. **Unlink** withdraws it. Mnemo never starts `claude` itself; the lead asks the desktop app to start the session.

## 6. When a host goes away

- Mnemo workers on the host continue if it is back within the outage budget (120 seconds by default). Otherwise they fail, and `team_resume` continues them.
- A Claude Code session keeps working on the host while the link is down.
- If the host restarts, its workers are interrupted, and `team_resume` starts them there again.
- When a worker merges, its checkout on the host is freed automatically.

## 7. Command reference

```
mnemo2-node pair --hub <host:port> --code <CODE> [--name <name>] [--config-dir <dir>]
mnemo2-node run --workspace <dir> [--slots <n>] [--config-dir <dir>]
mnemo2-node status [--config-dir <dir>]
mnemo2-node unpair [--config-dir <dir>]
```

- `pair` registers this computer with the hub using a pairing code.
- `run` connects to the hub and runs workers until you stop it.
- `status` shows the pairing and connection state.
- `unpair` forgets the pairing on this computer. It exits 0 even when the node was not paired.
- `--config-dir` sets where Mnemo keeps its files. Without it the node uses `$MNEMO_HOME`, else `~/.mnemo`.

The node's own files are in `<config dir>/node`: `node.json` (hub address, node id and token, the only secret the node keeps) and `node.log` (everything the node prints, rotated at 5 MiB).

Exit codes: 0 ok, 2 usage error, 3 not paired, 4 refused by the hub (unknown node, bad proof, bad or expired code), 5 protocol mismatch, 1 any other failure. A protocol mismatch means the host and hub run different Mnemo2 versions; update the older one.

## 8. Revoke and unpair

To remove a host from the hub, click **Revoke** in **Settings → Worker hosts**, then **Confirm revoke**. Its running workers stop and can be resumed with `team_resume`. Then run `mnemo2-node unpair` on the host to clear its side. If you unpair on the host first, revoke it on the hub as well so the entry disappears.
