// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Remote workers: the hub side of a Mnemo-model worker that runs on a paired
//! worker node (plan 9a2f4b67, phase 3 step 4).
//!
//! [`RemoteWorkers`] is the seam between the team runtime and the node link.
//! It never decides *whether* a worker goes remote — that is placement's job
//! (step 5). It owns the mechanics the rest of the team reads as if the worker
//! were local: one event demux per node, the git-bundle round trips that carry
//! a worker's commits into an ordinary hub shadow checkout, node-side check
//! runs, and the per-host suitability record that says whether a node can run
//! this project's checks at all.
//!
//! With no [`HostDirectory`] attached every method here is either unused or
//! answers from the local worktree, so an app that never pairs a node behaves
//! exactly as it did before phase 3.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use mnemo::headless::{SessionEvent, SessionId, SpawnOptions};
use mnemo::remote::hub::{EngineEventItem, HubServer, NodeHandle, RemoteSpawn, CALL_TIMEOUT};
use mnemo::remote::pairing::NodeRecord;
use mnemo::remote::repo::{
    BundleRef, CheckoutAnswer, CheckoutPrepare, CheckoutRelease, ChecksAnswer, ChecksRun,
    FetchBranch, FetchStart, HaveAnswer, HaveRequest, PushBegin, PushChunk, PushEnd, PushStarted,
    RepoTip, TipAnswer, BUNDLE_CHUNK, DEFAULT_NODE_SLOTS,
};
use mnemo::remote::{RemoteError, CODE_LINK_DOWN, CODE_TIMEOUT};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, Mutex};

use super::record::{RemoteCheckout, WorkerRecord};
use super::worker_session::{RemoteWorkerSession, SharedWorkerSession};

/// How many node events one per-node demux buffers before a slow reader lags.
///
/// The node's own ring keeps [`mnemo::remote::node::EVENT_RING`] events; this
/// buffer sits in front of every remote worker's pump on the hub, so it is the
/// pump's lag behaviour (a `RecvError::Lagged` notice) that a burst shows.
pub(crate) const NODE_EVENT_BUFFER: usize = 4096;

/// How long a node gets to mirror a freshly pushed model catalog before the
/// hub refuses to spawn on it (phase 2 must-do (a)).
const CATALOG_APPLY_WAIT: Duration = Duration::from_secs(30);

/// How long one node-side check run may take. The lead's own checks are four
/// heavy commands; six hours covers a cold build on a slow node.
const CHECKS_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// How long a sync may reuse its last answer without asking the node again.
const SYNC_REUSE: Duration = Duration::from_secs(5);

/// The branch a suitability qualification prepares on its node.
const QUALIFY_BRANCH: &str = "wt/agent-hostcheck";

/// The file the host suitability record lives in, under the team state dir.
const HOSTS_FILE: &str = "hosts.json";

/// Every bundle the hub builds or receives lands here, under the team state
/// dir (never the system temp folder).
const BUNDLES_DIR: &str = "remote/bundles";

/// One hub directory of paired worker nodes as the team sees it.
///
/// Implemented by the app (step 7's `AppHub` wraps the app's one [`HubServer`]);
/// the team only ever asks it for the listener and the pairing lifecycle, so a
/// test can stand a real hub in for it without the app.
#[async_trait::async_trait]
pub trait HostDirectory: Send + Sync {
    /// The running hub, or `None` while remoting is off (no node was ever
    /// paired).
    fn server(&self) -> Option<Arc<HubServer>>;
    /// The `host:port` a node dials in its `mnemo2-node pair` command.
    fn address(&self) -> Option<String>;
    /// Offer a one-time pairing code, optionally naming the node up front.
    async fn pair(&self, name: Option<&str>) -> Result<PairOffer, String>;
    /// Drop a paired node's record and cut its link.
    async fn revoke(&self, host: &str) -> Result<NodeRecord, String>;
}

/// One pairing offer as the lead shows it: the code, when it expires (Unix
/// milliseconds, like every other timestamp on the wire), and the hub address
/// the node's `pair` command names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairOffer {
    /// The one-time `XXXX-XXXX` code.
    pub code: String,
    /// When the code stops being redeemable, in Unix milliseconds.
    pub expires_ms: u64,
    /// The `host:port` the node dials.
    pub hub: String,
}

/// The 12-hex project key both ends name this project by.
///
/// The node keeps one repo and one pool per project key; the hub derives it
/// from the checkout root it serves, lowercased on Windows so a differently
/// cased path still names the same project.
pub(crate) fn project_key(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut text = canonical.to_string_lossy().into_owned();
    // Windows paths compare case-insensitively, so a differently cased root
    // must still name the same project. `MAIN_SEPARATOR` keeps this file free
    // of a platform `cfg` (tests/integration/policy.rs).
    if std::path::MAIN_SEPARATOR == '\\' {
        text = text.to_lowercase();
    }
    sha256_hex(text.as_bytes())[..12].to_string()
}

/// What one shadow sync did: the node tip the shadow now holds, the node tip's
/// uncommitted paths (a merge refuses to carry those), and whether the hub copy
/// moved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShadowSync {
    /// The branch tip on the node, as the shadow now holds it.
    pub tip: String,
    /// `git status --porcelain` lines from the node, capped there at 50.
    pub dirty: Vec<String>,
    /// Whether the shadow checkout moved to a new commit.
    pub changed: bool,
}

/// What the hub knows about one host's ability to run this project's checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Qualification {
    /// Never checked on this host, or the project's checks changed since.
    #[default]
    Pending,
    /// The last measured run got the checks running on this host.
    Passed,
    /// A check command could not run at all here (wrong shell, missing tool).
    Failed,
}

/// One host's qualification for one project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProjectState {
    /// The hash of the check commands the measurement ran.
    pub checks_hash: String,
    /// The outcome.
    pub qualification: Qualification,
    /// Why it failed, for `team_hosts list`.
    pub reason: Option<String>,
    /// When the measurement landed, in Unix milliseconds.
    pub updated_ms: u64,
}

/// The persisted suitability record: `<state_dir>/hosts.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostSuitability {
    /// One entry per node id measured for this project.
    #[serde(default)]
    hosts: BTreeMap<String, HostProjectState>,
}

impl HostSuitability {
    /// Read the record beside the team ledger (`hosts.json`); a missing or
    /// unreadable file means "nothing measured yet".
    fn load(state_dir: &Path) -> Self {
        std::fs::read_to_string(state_dir.join(HOSTS_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write the record back, temp file then rename so a crash never leaves a
    /// half-written file.
    fn save(&self, state_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(state_dir)
            .map_err(|error| format!("could not create {}: {error}", state_dir.display()))?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("could not encode the host suitability: {error}"))?;
        let path = state_dir.join(HOSTS_FILE);
        let temp = state_dir.join(format!("{HOSTS_FILE}.tmp"));
        std::fs::write(&temp, text)
            .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
        std::fs::rename(&temp, &path)
            .map_err(|error| format!("could not replace {}: {error}", path.display()))
    }
}

/// Whether a failing check command failed because the host cannot run it at
/// all, rather than because the project is broken at that tip.
///
/// Exit 127 (POSIX "command not found") and 9009 (cmd's "not recognized") are
/// the shell's own answer; the texts are what Windows' cmd, PowerShell, Git
/// Bash and a missing executable print in the child's output.
pub(crate) fn environmental_failure(exit_code: Option<i64>, text: &str) -> bool {
    if matches!(exit_code, Some(127) | Some(9009)) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    const NEEDLES: [&str; 5] = [
        "command not found",
        "is not recognized as an internal or external command",
        "is not recognized as the name of a cmdlet",
        "program not found",
        "cannot find the file specified",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

/// Whether these commands only run on Windows: PowerShell, cmd, or a batch or
/// PowerShell script file.
///
/// The rule reads the check commands ONLY — never the brief or the plan text,
/// which in this repository routinely names PowerShell and Windows-only
/// compiler gates and would pin every dispatch to a Windows host.
pub fn requires_windows(commands: &[String]) -> bool {
    const WINDOWS_SHELLS: [&str; 5] = [
        "powershell",
        "powershell.exe",
        "pwsh.exe",
        "cmd",
        "cmd.exe",
    ];
    commands.iter().any(|command| {
        let first = command
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches('"')
            .to_ascii_lowercase();
        if WINDOWS_SHELLS.contains(&first.as_str()) {
            return true;
        }
        // `pwsh` alone runs on macOS and Linux too (PowerShell 7), so it is
        // Windows-only only when it runs a PowerShell script; the `.exe`
        // spelling is how a Windows config runs it, so that one names Windows
        // directly. The file suffixes match a whole token, never the middle of
        // one: `bats tests/e2e.bats` is not a Windows batch file.
        command.split_whitespace().any(|token| {
            let token = token
                .trim_matches('"')
                .trim_end_matches([',', ';'])
                .to_ascii_lowercase();
            [".ps1", ".bat", ".cmd"]
                .iter()
                .any(|suffix| token.ends_with(suffix))
        })
    })
}

/// The hash a qualification measures: the project's check commands as the
/// project root lists them.
pub(crate) fn checks_hash(commands: &[String]) -> String {
    sha256_hex(commands.join("\n").as_bytes())
}

/// One run's verdict, as `note_check` wants it: did any command pass, and is
/// there evidence that a failure was the host's fault.
fn summarize_checks(answer: &ChecksAnswer) -> (bool, Option<String>) {
    let entries = answer
        .result
        .data
        .as_ref()
        .and_then(|data| data.get("checks"))
        .and_then(Value::as_array);
    let Some(entries) = entries else {
        return (false, None);
    };
    let mut any_passed = false;
    let mut environmental: Option<String> = None;
    for entry in entries {
        let exit_code = entry.get("exit_code").and_then(Value::as_i64);
        if exit_code == Some(0) {
            any_passed = true;
            continue;
        }
        if environmental.is_none() && environmental_failure(exit_code, &answer.result.output) {
            let command = entry
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("a check command");
            environmental = Some(format!("`{command}` could not run on this host"));
        }
    }
    (any_passed, environmental)
}

/// Lowercase hex of `bytes`' SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Run one git command in `root` and return its trimmed stdout.
pub(super) async fn git_value(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Ask `node` for `method` and decode its answer as `T`.
async fn call_host<T: DeserializeOwned>(
    node: &NodeHandle,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<T, RemoteError> {
    let value = node.call(method, params, timeout).await?;
    serde_json::from_value(value)
        .map_err(|error| RemoteError::internal(format!("bad {method} answer: {error}")))
}

/// The text every remote failure carries: which host it happened on.
fn host_error(host: &str, error: RemoteError) -> String {
    format!("host '{host}': {error}")
}

/// The text a placement may read: whether a failed call means the node is
/// away rather than wrong.
pub(crate) fn host_offline(error: &RemoteError) -> bool {
    error.code == CODE_LINK_DOWN || error.code == CODE_TIMEOUT
}

/// No host directory is attached yet (remoting is off).
fn no_hosts() -> String {
    "no worker host is paired with this Mnemo2 instance".to_string()
}

/// The team's remote-worker seam: host lookup, the per-node event demux, the
/// git-bundle round trips, node-side checks, and host suitability.
///
/// One instance lives on [`super::TeamRuntime`]. It holds the team's own
/// `records` and `live` maps so the monitor and the reconciler can read them
/// without a back-reference to the runtime, and it never holds those locks
/// across a link call.
pub struct RemoteWorkers {
    /// The app's host directory, once the runtime is attached to one.
    hosts: StdRwLock<Option<Arc<dyn HostDirectory>>>,
    /// The project root the hub copies commits into.
    workspace_root: PathBuf,
    /// The team's state dir (bundles and `hosts.json` live under it).
    state_dir: PathBuf,
    /// The 12-hex project key both ends name this project by.
    project: String,
    /// The team's worker records.
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    /// The team's live sessions.
    live: Arc<Mutex<HashMap<String, super::LiveWorker>>>,
    /// One demux per node id: every session on that node subscribes to it.
    node_events: StdMutex<HashMap<String, broadcast::Sender<SessionEvent>>>,
    /// One sync lock per worker id, so two syncs never race a fetch.
    sync_locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Serializes the fetch into the project repo (bundles land one at a time).
    fetch_lock: Mutex<()>,
    /// The last `(tip, when)` a sync answered per worker, for the fast path.
    last_sync: StdMutex<HashMap<String, (String, Instant)>>,
    /// The per-host qualification record for this project.
    suitability: StdMutex<HostSuitability>,
    /// The node checkouts a qualification is using right now (node id →
    /// checkout), so the reconciler never releases one under it.
    qualifying: StdMutex<HashMap<String, String>>,
    /// How long a node may stay away before its worker fails. The production
    /// value is [`mnemo::remote::HUB_OUTAGE_BUDGET`]; tests shorten it.
    outage_budget: StdMutex<Duration>,
    /// Sessions seen without a live entry on the previous reconcile pass, so a
    /// session that is still spawning is never closed on its first sighting.
    seen_orphans: StdMutex<HashSet<(String, SessionId)>>,
    /// The `(node_id, branch)` checkouts a dispatch is preparing right now.
    /// The record that names them is stored only after `prepare_checkout`
    /// returns, so the reconciler must leave them alone until then.
    reservations: StdMutex<HashSet<(String, String)>>,
}

impl RemoteWorkers {
    /// Build the seam for `workspace_root`; no host directory is attached yet,
    /// so every path through it is local.
    pub(super) fn new(
        workspace_root: PathBuf,
        state_dir: PathBuf,
        records: Arc<Mutex<Vec<WorkerRecord>>>,
        live: Arc<Mutex<HashMap<String, super::LiveWorker>>>,
    ) -> Self {
        let project = project_key(&workspace_root);
        let suitability = HostSuitability::load(&state_dir);
        Self {
            hosts: StdRwLock::new(None),
            workspace_root,
            state_dir,
            project,
            records,
            live,
            node_events: StdMutex::new(HashMap::new()),
            sync_locks: StdMutex::new(HashMap::new()),
            fetch_lock: Mutex::new(()),
            last_sync: StdMutex::new(HashMap::new()),
            suitability: StdMutex::new(suitability),
            qualifying: StdMutex::new(HashMap::new()),
            outage_budget: StdMutex::new(mnemo::remote::HUB_OUTAGE_BUDGET),
            seen_orphans: StdMutex::new(HashSet::new()),
            reservations: StdMutex::new(HashSet::new()),
        }
    }

    /// Attach the app's host directory: from here on the team may place
    /// workers on a node.
    pub fn attach(&self, hosts: Arc<dyn HostDirectory>) {
        *self
            .hosts
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(hosts);
    }

    /// The running hub, or `None` while remoting is off.
    pub fn server(&self) -> Option<Arc<HubServer>> {
        self.hosts
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .and_then(|hosts| hosts.server())
    }

    /// The 12-hex project key every repo call names.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// How long a node may stay away before its worker fails.
    pub fn outage_budget(&self) -> Duration {
        *self
            .outage_budget
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Shorten the outage budget so a test need not wait two minutes.
    #[cfg(test)]
    pub fn set_outage_budget(&self, budget: Duration) {
        *self
            .outage_budget
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = budget;
    }

    /// One paired node by its name or its 32-hex id.
    pub fn host(&self, name_or_id: &str) -> Result<NodeRecord, String> {
        let server = self.server().ok_or_else(no_hosts)?;
        server
            .nodes()
            .list()?
            .into_iter()
            .find(|record| record.node_id == name_or_id || record.name == name_or_id)
            .ok_or_else(|| format!("no worker host named '{name_or_id}'"))
    }

    /// Keep the reconciler's hands off `branch`'s checkout on `node_id` while
    /// a dispatch prepares it.
    ///
    /// The worker record that will name the checkout is stored only after
    /// `prepare_checkout` returns; without the reservation the reconciler
    /// could list the fresh `wt/agent-*` checkout as an orphan and release it
    /// under the dispatch.
    pub fn reserve(&self, node_id: &str, branch: &str) {
        self.reservations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert((node_id.to_string(), branch.to_string()));
    }

    /// Drop a reservation once the record names the checkout, or the dispatch
    /// failed before there was one.
    pub fn release_reservation(&self, node_id: &str, branch: &str) {
        self.reservations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&(node_id.to_string(), branch.to_string()));
    }

    /// Whether a dispatch is preparing `branch`'s checkout on `node_id`.
    pub fn is_reserved(&self, node_id: &str, branch: &str) -> bool {
        self.reservations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(&(node_id.to_string(), branch.to_string()))
    }

    /// How many live workers run on `node_id`.
    pub async fn busy_on(&self, node_id: &str) -> usize {
        // Never hold `records` and `live` together: the delivery path takes
        // `records` while it holds `live`. Copy the live ids, drop that lock,
        // then read the records.
        let live_ids: Vec<String> = self.live.lock().await.keys().cloned().collect();
        let records = self.records.lock().await;
        live_ids
            .iter()
            .filter(|id| {
                records.iter().any(|record| {
                    &record.id == *id
                        && record
                            .remote
                            .as_ref()
                            .is_some_and(|remote| remote.node_id == node_id)
                })
            })
            .count()
    }

    /// How many pool slots `node` advertises, and its OS.
    pub async fn slots_of(&self, node: &NodeRecord) -> usize {
        let Some(server) = self.server() else {
            return DEFAULT_NODE_SLOTS;
        };
        match server.node(&node.node_id).status().await {
            Ok(status) => status
                .get("slots")
                .and_then(Value::as_u64)
                .map(|slots| usize::try_from(slots).unwrap_or(DEFAULT_NODE_SLOTS))
                .filter(|slots| *slots > 0)
                .unwrap_or(DEFAULT_NODE_SLOTS),
            Err(_) => DEFAULT_NODE_SLOTS,
        }
    }

    /// Make sure `node` mirrors the hub's current model catalog before a
    /// dispatch pins an endpoint/model on it (phase 2 must-do (a)).
    ///
    /// A node refuses `session.spawn` for a model its applied catalog does not
    /// know, so the hub pushes the catalog and waits for the node's
    /// `catalog_revision` to catch up.
    pub async fn ensure_catalog(
        &self,
        node: &NodeHandle,
        name: &str,
    ) -> Result<(), String> {
        let server = self.server().ok_or_else(no_hosts)?;
        let wanted = server.catalog_revision();
        let mut status = node.status().await.map_err(|error| host_error(name, error))?;
        let applied = status
            .get("catalog_revision")
            .and_then(Value::as_str)
            .map(str::to_string);
        if applied.as_deref() == Some(wanted.as_str()) {
            return Ok(());
        }
        server.push_catalog();
        let deadline = Instant::now() + CATALOG_APPLY_WAIT;
        loop {
            if applied.as_deref() == Some(wanted.as_str()) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "host '{name}' did not apply the current model catalog within {} s",
                    CATALOG_APPLY_WAIT.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            status = node.status().await.map_err(|error| host_error(name, error))?;
            let applied = status
                .get("catalog_revision")
                .and_then(Value::as_str)
                .map(str::to_string);
            if applied.as_deref() == Some(wanted.as_str()) {
                return Ok(());
            }
        }
    }

    /// Send `sha`'s history to `node` as a bundle unless it already has it.
    ///
    /// The bundle carries one temporary ref (`refs/mnemo/push/<sha12>`) and
    /// excludes every ref tip the node reported, so an incremental push sends
    /// only the commits the node is missing. The temporary ref and the bundle
    /// file are removed afterwards, whatever happened.
    pub async fn push_commit(&self, node: &NodeHandle, sha: &str) -> Result<(), String> {
        if sha.trim().is_empty() {
            return Err("cannot push an empty commit to a worker host".to_string());
        }
        let had: HaveAnswer = call_host(
            node,
            mnemo::remote::repo::REPO_HAVE,
            json!(HaveRequest {
                project: self.project.clone(),
                want: vec![sha.to_string()],
            }),
            CALL_TIMEOUT,
        )
        .await
        .map_err(|error| format!("the worker host: {error}"))?;
        if had.missing.is_empty() {
            return Ok(());
        }
        let short = &sha[..sha.len().min(12)];
        let reference = format!("refs/mnemo/push/{short}");
        let bundle = self.bundle_dir()?.join(format!("push-{short}.bundle"));
        let outcome = self
            .push_bundle_bytes(node, sha, &reference, &bundle, &had.tips)
            .await;
        let _ = git_value(&self.workspace_root, &["update-ref", "-d", &reference]).await;
        let _ = std::fs::remove_file(&bundle);
        outcome
    }

    /// Build the bundle, upload it in [`BUNDLE_CHUNK`] pieces, and land its ref.
    async fn push_bundle_bytes(
        &self,
        node: &NodeHandle,
        sha: &str,
        reference: &str,
        bundle: &Path,
        excludes: &[String],
    ) -> Result<(), String> {
        git_value(&self.workspace_root, &["update-ref", reference, sha]).await?;
        let path = bundle.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["bundle", "create", &path, reference];
        let exclusions: Vec<String> = excludes.iter().map(|tip| format!("^{tip}")).collect();
        for exclusion in &exclusions {
            args.push(exclusion.as_str());
        }
        git_value(&self.workspace_root, &args).await?;
        let bytes = std::fs::read(bundle)
            .map_err(|error| format!("could not read {}: {error}", bundle.display()))?;
        let size = bytes.len() as u64;
        let digest = sha256_hex(&bytes);
        let started: PushStarted = call_host(
            node,
            mnemo::remote::repo::REPO_PUSH_BEGIN,
            json!(PushBegin {
                project: self.project.clone(),
                size,
                sha256: digest,
            }),
            CALL_TIMEOUT,
        )
        .await
        .map_err(|error| format!("the worker host: {error}"))?;
        let mut offset = 0usize;
        for piece in bytes.chunks(BUNDLE_CHUNK) {
            let data = base64::engine::general_purpose::STANDARD.encode(piece);
            let _: Value = call_host(
                node,
                mnemo::remote::repo::REPO_PUSH_CHUNK,
                json!(PushChunk {
                    upload: started.upload.clone(),
                    offset: offset as u64,
                    data,
                }),
                CALL_TIMEOUT,
            )
            .await
            .map_err(|error| format!("the worker host: {error}"))?;
            offset += piece.len();
        }
        let _: Value = call_host(
            node,
            mnemo::remote::repo::REPO_PUSH_END,
            json!(PushEnd {
                upload: started.upload,
                refs: vec![BundleRef {
                    name: reference.to_string(),
                    sha: sha.to_string(),
                }],
            }),
            CALL_TIMEOUT,
        )
        .await
        .map_err(|error| format!("the worker host: {error}"))?;
        Ok(())
    }

    /// Where bundle files live while they travel.
    fn bundle_dir(&self) -> Result<PathBuf, String> {
        let dir = self.state_dir.join(BUNDLES_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
        Ok(dir)
    }

    /// Put one checkout of `node_id` on `branch` at `base`, after making sure
    /// the node has `base`'s history.
    pub async fn prepare_checkout(
        &self,
        node_id: &str,
        branch: &str,
        base: &str,
        keep_existing: bool,
    ) -> Result<RemoteCheckout, String> {
        let server = self.server().ok_or_else(no_hosts)?;
        let record = self.host(node_id)?;
        let node = server.node(&record.node_id);
        self.push_commit(&node, base).await?;
        let answer: CheckoutAnswer = call_host(
            &node,
            mnemo::remote::repo::CHECKOUT_PREPARE,
            json!(CheckoutPrepare {
                project: self.project.clone(),
                branch: branch.to_string(),
                base: base.to_string(),
                keep_existing,
            }),
            CALL_TIMEOUT,
        )
        .await
        .map_err(|error| host_error(&record.name, error))?;
        Ok(RemoteCheckout {
            node_id: record.node_id,
            node_name: record.name,
            os: record.os,
            project: self.project.clone(),
            checkout: answer.checkout,
            path: answer.path,
            synced_tip: None,
            offline_since_ms: None,
        })
    }

    /// Spawn one worker session in `remote`'s node checkout.
    ///
    /// The hub's worktree path means nothing on the node, so the options go out
    /// with `worktree: None`. The session's events come from the per-node
    /// demux; the caller has already run [`Self::ensure_catalog`] and
    /// [`Self::prepare_checkout`] (placement owns that ordering).
    pub async fn spawn_session(
        &self,
        remote: &RemoteCheckout,
        options: SpawnOptions,
    ) -> Result<SharedWorkerSession, String> {
        let server = self.server().ok_or_else(no_hosts)?;
        let node = server.node(&remote.node_id);
        let events = self.demux(&node, &remote.node_name, &remote.node_id).await?;
        let mut options = options;
        options.worktree = None;
        let id = node
            .spawn_session(RemoteSpawn {
                options,
                checkout: Some(remote.checkout.clone()),
            })
            .await
            .map_err(|error| host_error(&remote.node_name, error))?;
        Ok(Arc::new(RemoteWorkerSession::new(
            node,
            remote.node_name.clone(),
            id,
            events,
        )) as SharedWorkerSession)
    }

    /// The one event demux of `node_id`, created on first use.
    ///
    /// Every session on the node subscribes to this sender and its pump filters
    /// by `is_from`, exactly as on the shared local bus. A gap in the node's
    /// ring is logged, not fatal: the pump's own lag handling covers the rest.
    async fn demux(
        &self,
        node: &NodeHandle,
        name: &str,
        node_id: &str,
    ) -> Result<broadcast::Sender<SessionEvent>, String> {
        if let Some(sender) = self
            .node_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(node_id)
        {
            return Ok(sender.clone());
        }
        // Fetch the cursor before taking the map lock: this is a link call and
        // the lock must never span one.
        let last = node
            .last_event_seq()
            .await
            .map_err(|error| host_error(name, error))?;
        let mut streams = self
            .node_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(sender) = streams.get(node_id) {
            return Ok(sender.clone());
        }
        let (sender, _) = broadcast::channel(NODE_EVENT_BUFFER);
        let mut items = node.events_from(last.saturating_add(1));
        let forward = sender.clone();
        let label = name.to_string();
        tokio::spawn(async move {
            while let Some(item) = items.recv().await {
                match item {
                    EngineEventItem::Event { event, .. } => {
                        // A closed bus (every session on the node gone) is not
                        // fatal: the next session subscribes again.
                        let _ = forward.send(event);
                    }
                    EngineEventItem::Gap { from, to } => {
                        eprintln!("team: host {label} lost events {from}..={to}");
                    }
                }
            }
        });
        streams.insert(node_id.to_string(), sender.clone());
        Ok(sender)
    }

    /// What one node checkout currently holds.
    pub async fn node_tip(&self, remote: &RemoteCheckout) -> Result<TipAnswer, String> {
        let server = self.server().ok_or_else(no_hosts)?;
        let node = server.node(&remote.node_id);
        self.tip_via(&node, remote)
            .await
            .map_err(|error| host_error(&remote.node_name, error))
    }

    /// `repo.tip` with the error still typed, so a caller can tell a missing
    /// host from a wrong answer.
    async fn tip_via(
        &self,
        node: &NodeHandle,
        remote: &RemoteCheckout,
    ) -> Result<TipAnswer, RemoteError> {
        call_host(
            node,
            mnemo::remote::repo::REPO_TIP,
            json!(RepoTip {
                checkout: remote.checkout.clone(),
            }),
            CALL_TIMEOUT,
        )
        .await
    }

    /// Copy the worker's branch from its node into the hub shadow checkout.
    ///
    /// A local worker answers its worktree tip; a remote one fetches the node's
    /// branch as a bundle into `refs/mnemo/remote/<id>`, then moves the shadow
    /// (an ordinary hub pool checkout) onto it. `force` skips the five-second
    /// reuse window. The hub copy is never overwritten while it has local
    /// changes.
    pub async fn sync_shadow(
        &self,
        record: &WorkerRecord,
        force: bool,
    ) -> Result<ShadowSync, String> {
        let Some(remote) = record.remote.as_ref() else {
            let tip = crate::mnemo2::phase3::tip(&record.worktree).await?;
            return Ok(ShadowSync {
                tip,
                dirty: Vec::new(),
                changed: false,
            });
        };
        let gate = self.sync_lock(&record.id);
        let _held = gate.lock().await;
        let server = self.server().ok_or_else(no_hosts)?;
        let node = server.node(&remote.node_id);
        if !force {
            let cached = self
                .last_sync
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&record.id)
                .cloned();
            if let Some((tip, at)) = cached {
                if at.elapsed() < SYNC_REUSE {
                    if let Ok(answer) = self.tip_via(&node, remote).await {
                        if answer.tip == tip {
                            return Ok(ShadowSync {
                                tip,
                                dirty: answer.dirty,
                                changed: false,
                            });
                        }
                    }
                }
            }
        }
        let answer = match self.tip_via(&node, remote).await {
            Ok(answer) => answer,
            Err(error) if host_offline(&error) => {
                return Err(format!(
                    "host '{}' is offline; Mnemo cannot read worker '{}'s latest commits",
                    remote.node_name, record.id
                ));
            }
            Err(error) => return Err(host_error(&remote.node_name, error)),
        };
        let shadow_tip = crate::mnemo2::phase3::tip(&record.worktree)
            .await
            .unwrap_or_default();
        if answer.tip == shadow_tip {
            self.remember_sync(&record.id, &answer.tip);
            self.note_synced(record, &answer.tip).await;
            return Ok(ShadowSync {
                tip: answer.tip,
                dirty: answer.dirty,
                changed: false,
            });
        }
        let start = self.fetch_into_shadow(&node, record, remote).await?;
        let local = git_value(&record.worktree, &["status", "--porcelain"]).await?;
        if !local.trim().is_empty() {
            return Err(format!(
                "the hub copy {} of worker '{}' has local changes; Mnemo will not overwrite it",
                record.worktree.display(),
                record.id
            ));
        }
        let branch = git_value(&record.worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        if branch.trim() != record.branch {
            git_value(&record.worktree, &["checkout", "-B", &record.branch]).await?;
        }
        git_value(&record.worktree, &["reset", "--hard", &start.tip]).await?;
        self.remember_sync(&record.id, &start.tip);
        self.note_synced(record, &start.tip).await;
        Ok(ShadowSync {
            tip: start.tip,
            dirty: start.dirty,
            changed: true,
        })
    }

    /// Stream `repo.fetch_branch` into a bundle and fetch it into the project
    /// repo as `refs/mnemo/remote/<id>`; the caller resets the shadow onto it.
    ///
    /// When the node answers `unchanged` (the branch tip is one of the haves
    /// the hub sent, so there is nothing to carry), no bundle is written and no
    /// digest is checked; the caller still moves the shadow onto `start.tip`.
    async fn fetch_into_shadow(
        &self,
        node: &NodeHandle,
        record: &WorkerRecord,
        remote: &RemoteCheckout,
    ) -> Result<FetchStart, String> {
        let haves = self.fetch_haves(record, remote).await;
        let opened = node
            .call_stream(
                mnemo::remote::repo::REPO_FETCH_BRANCH,
                json!(FetchBranch {
                    checkout: remote.checkout.clone(),
                    branch: record.branch.clone(),
                    haves,
                }),
                CALL_TIMEOUT,
            )
            .await
            .map_err(|error| host_error(&remote.node_name, error))?;
        let (start, mut pieces) = opened;
        let start: FetchStart = serde_json::from_value(start)
            .map_err(|error| format!("host '{}': bad fetch start: {error}", remote.node_name))?;
        if start.unchanged {
            return Ok(start);
        }
        let short = &start.tip[..start.tip.len().min(7)];
        let bundle = self
            .bundle_dir()?
            .join(format!("{}-{short}.bundle", record.id));
        let mut bytes: Vec<u8> = Vec::with_capacity(usize::try_from(start.size).unwrap_or(0));
        while let Some(piece) = pieces.next().await {
            let piece = piece.map_err(|error| host_error(&remote.node_name, error))?;
            let Some(data) = piece.d.get("data").and_then(Value::as_str) else {
                continue;
            };
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|error| format!("host '{}': bad bundle piece: {error}", remote.node_name))?;
            bytes.extend_from_slice(&decoded);
        }
        if bytes.len() as u64 != start.size || sha256_hex(&bytes) != start.sha256 {
            let _ = std::fs::remove_file(&bundle);
            return Err(format!(
                "host '{}': the bundle for worker '{}' did not match what the host promised",
                remote.node_name, record.id
            ));
        }
        std::fs::write(&bundle, &bytes)
            .map_err(|error| format!("could not write {}: {error}", bundle.display()))?;
        let path = bundle.to_string_lossy().into_owned();
        let target = format!("+refs/heads/{}:refs/mnemo/remote/{}", record.branch, record.id);
        let fetched = {
            let _fetch = self.fetch_lock.lock().await;
            let verified = git_value(&self.workspace_root, &["bundle", "verify", &path]).await;
            match verified {
                Ok(_) => {
                    git_value(
                        &self.workspace_root,
                        &["fetch", "--no-tags", &path, &target],
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        };
        let _ = std::fs::remove_file(&bundle);
        fetched?;
        Ok(start)
    }

    /// The commits the node's bundle may exclude: the shadow tip and the
    /// worker's base commit, when the hub has them.
    async fn fetch_haves(&self, record: &WorkerRecord, _remote: &RemoteCheckout) -> Vec<String> {
        let mut haves = Vec::new();
        if let Ok(tip) = crate::mnemo2::phase3::tip(&record.worktree).await {
            if !tip.is_empty() {
                haves.push(tip);
            }
        }
        if let Ok(head) = git_value(&self.workspace_root, &["rev-parse", "HEAD"]).await {
            if !head.is_empty() && !haves.contains(&head) {
                haves.push(head);
            }
        }
        if let Some(base) = record.base_commit.as_deref().map(str::trim) {
            if !base.is_empty() && !haves.iter().any(|have| have == base) {
                haves.push(base.to_string());
            }
        }
        haves
    }

    /// Remember the tip a sync answered, for the five-second fast path.
    fn remember_sync(&self, worker_id: &str, tip: &str) {
        self.last_sync
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(worker_id.to_string(), (tip.to_string(), Instant::now()));
    }

    /// Record the tip the hub copy now holds on the worker's record.
    async fn note_synced(&self, record: &WorkerRecord, tip: &str) {
        let mut records = self.records.lock().await;
        if let Some(live) = records.iter_mut().find(|worker| worker.id == record.id) {
            if let Some(remote) = live.remote.as_mut() {
                remote.synced_tip = Some(tip.to_string());
            }
        }
    }

    /// The lock that serializes two syncs of one worker.
    fn sync_lock(&self, worker_id: &str) -> Arc<Mutex<()>> {
        self.sync_locks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(worker_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Run the project's checks in `remote`'s node checkout at `tip`.
    pub async fn run_checks(
        &self,
        remote: &RemoteCheckout,
        commands: &[String],
        tip: &str,
    ) -> Result<ChecksAnswer, String> {
        let server = self.server().ok_or_else(no_hosts)?;
        let node = server.node(&remote.node_id);
        call_host(
            &node,
            mnemo::remote::repo::CHECKS_RUN,
            json!(ChecksRun {
                checkout: remote.checkout.clone(),
                commands: commands.to_vec(),
                tip: tip.to_string(),
            }),
            CHECKS_TIMEOUT,
        )
        .await
        .map_err(|error| host_error(&remote.node_name, error))
    }

    /// One arbitrary call into `remote`'s node (step 6's proof runs).
    pub async fn call_node(
        &self,
        remote: &RemoteCheckout,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let server = self.server().ok_or_else(no_hosts)?;
        let node = server.node(&remote.node_id);
        node.call(method, params, timeout)
            .await
            .map_err(|error| host_error(&remote.node_name, error))
    }

    /// Hand `remote`'s checkout back to the node's pool.
    pub async fn release(&self, remote: &RemoteCheckout) {
        let Some(server) = self.server() else {
            return;
        };
        let node = server.node(&remote.node_id);
        if let Err(error) = node
            .call(
                mnemo::remote::repo::CHECKOUT_RELEASE,
                json!(CheckoutRelease {
                    checkout: remote.checkout.clone(),
                }),
                CALL_TIMEOUT,
            )
            .await
        {
            eprintln!(
                "team: could not release checkout {} on host '{}': {error}",
                remote.checkout, remote.node_name
            );
        }
    }

    /// The checkout a qualification is using on `node_id` right now, if any.
    pub fn qualifying_checkout(&self, node_id: &str) -> Option<String> {
        self.qualifying
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(node_id)
            .cloned()
    }

    /// Measure, in the background and at most once per node, whether `node_id`
    /// can run this project's checks.
    ///
    /// The check runs on a plain `wt/agent-hostcheck` checkout at the lead's
    /// current tip with the project's own commands; the answer lands in
    /// `hosts.json` and placement reads it through [`Self::qualification`].
    pub fn qualify(self: &Arc<Self>, node_id: &str) {
        {
            let mut qualifying = self
                .qualifying
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if qualifying.contains_key(node_id) {
                return;
            }
            qualifying.insert(node_id.to_string(), String::new());
        }
        let workers = Arc::clone(self);
        let node_id = node_id.to_string();
        tokio::spawn(async move {
            if let Err(error) = workers.run_qualification(&node_id).await {
                eprintln!("team: host qualification on {node_id} failed: {error}");
            }
            workers
                .qualifying
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&node_id);
        });
    }

    /// One qualification run: prepare, measure, record, release.
    async fn run_qualification(&self, node_id: &str) -> Result<(), String> {
        let commands = super::spawn::load_check_commands(&self.workspace_root);
        let hash = checks_hash(&commands);
        let base = git_value(&self.workspace_root, &["rev-parse", "HEAD"]).await?;
        let remote = self
            .prepare_checkout(node_id, QUALIFY_BRANCH, &base, false)
            .await?;
        self.qualifying
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(node_id.to_string(), remote.checkout.clone());
        let outcome = self.run_checks(&remote, &commands, &base).await;
        self.release(&remote).await;
        match outcome {
            Ok(answer) => {
                let (any_passed, environmental) = summarize_checks(&answer);
                self.note_check(node_id, &hash, any_passed, environmental);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Record what one measured run says about `node_id` for these commands.
    ///
    /// An environmental failure — a shell or tool the host does not have —
    /// disqualifies the host; anything else means the host can run the
    /// project's checks (red tests at the lead's tip are the project's fault,
    /// not the host's). `any_passed` only shapes the reason text.
    pub fn note_check(
        &self,
        node_id: &str,
        checks_hash: &str,
        any_passed: bool,
        environmental: Option<String>,
    ) {
        let (qualification, reason) = match environmental {
            Some(reason) => (Qualification::Failed, Some(reason)),
            None if any_passed => (Qualification::Passed, None),
            None => (
                Qualification::Passed,
                Some(
                    "no check command passed at the qualifying tip; the host itself runs them"
                        .to_string(),
                ),
            ),
        };
        let state = HostProjectState {
            checks_hash: checks_hash.to_string(),
            qualification,
            reason,
            updated_ms: super::unix_ms(),
        };
        let mut suitability = self
            .suitability
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        suitability.hosts.insert(node_id.to_string(), state);
        if let Err(error) = suitability.save(&self.state_dir) {
            eprintln!("team: could not save the host suitability record: {error}");
        }
    }

    /// What is known about `node_id` for these check commands: a changed hash
    /// (or no measurement) is `Pending` again.
    pub fn qualification(&self, node_id: &str, checks_hash: &str) -> Qualification {
        self.suitability
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .hosts
            .get(node_id)
            .filter(|state| state.checks_hash == checks_hash)
            .map(|state| state.qualification)
            .unwrap_or_default()
    }

    /// The full record, for `team_hosts list` (step 5).
    pub fn host_states(&self) -> BTreeMap<String, HostProjectState> {
        self.suitability
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .hosts
            .clone()
    }

    /// Whether `branch`'s checkout on `node_id` must survive the reconciler
    /// because a qualification holds (or is about to hold) that node.
    pub fn qualification_protects(&self, node_id: &str, branch: &str) -> bool {
        branch == QUALIFY_BRANCH
            && self
                .qualifying
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains_key(node_id)
    }

    /// The sessions that were already orphans on the previous reconcile pass.
    pub fn orphans(&self) -> HashSet<(String, SessionId)> {
        self.seen_orphans
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Replace the orphan set with this pass's sightings.
    pub fn replace_orphans(&self, orphans: HashSet<(String, SessionId)>) {
        *self
            .seen_orphans
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = orphans;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use mnemo::headless::session::ModelSwitch;
    use mnemo::headless::{EventReceiver, SessionCommand, SessionId, SessionSnapshot};
    use mnemo::remote::catalog::ModelCatalog;
    use mnemo::remote::hub::{HubOptions, HubServices};
    use mnemo::remote::node::{self, NodeConfig};

    use mnemo::route_question::Strikes;

    use crate::mnemo2::team::{
        LiveWorker, SharedWorkerSession, TeamRuntime, TeamRuntimeOptions,
    };
    use crate::mnemo2::team::worker_session::WorkerSession;
    use crate::mnemo2::team::WorkerStatus;

    /// A hub with no model clients and no memory: the repo, checkout and check
    /// paths under test never need either.
    struct StubServices {
        catalog: ModelCatalog,
    }

    impl HubServices for StubServices {
        fn model_client(&self, _model: &mnemo::config::ModelRef) -> Option<Arc<dyn mnemo::provider::LlmClient>> {
            None
        }

        fn memory(&self) -> Option<Arc<dyn mnemo::memory::MemoryStoreTrait>> {
            None
        }

        fn catalog(&self) -> ModelCatalog {
            self.catalog.clone()
        }
    }

    /// The team's view of one real hub, for tests.
    struct TestHosts {
        server: Arc<HubServer>,
        address: String,
    }

    #[async_trait::async_trait]
    impl HostDirectory for TestHosts {
        fn server(&self) -> Option<Arc<HubServer>> {
            Some(self.server.clone())
        }

        fn address(&self) -> Option<String> {
            Some(self.address.clone())
        }

        async fn pair(&self, name: Option<&str>) -> Result<PairOffer, String> {
            let code = self.server.pairing().issue_code(name.unwrap_or("node"));
            Ok(PairOffer {
                code: code.code,
                expires_ms: code.expires_ms,
                hub: self.address.clone(),
            })
        }

        async fn revoke(&self, host: &str) -> Result<NodeRecord, String> {
            let record = self
                .server
                .nodes()
                .list()?
                .into_iter()
                .find(|record| record.node_id == host || record.name == host)
                .ok_or_else(|| format!("no worker host named '{host}'"))?;
            self.server.revoke(&record.node_id).map_err(|error| error.to_string())?;
            Ok(record)
        }
    }

    /// A host directory whose hub appears only after it was attached.
    struct LateHosts {
        server: StdMutex<Option<Arc<HubServer>>>,
        address: String,
    }

    #[async_trait::async_trait]
    impl HostDirectory for LateHosts {
        fn server(&self) -> Option<Arc<HubServer>> {
            self.server
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }

        fn address(&self) -> Option<String> {
            Some(self.address.clone())
        }

        async fn pair(&self, _name: Option<&str>) -> Result<PairOffer, String> {
            Err("unused".to_string())
        }

        async fn revoke(&self, _host: &str) -> Result<NodeRecord, String> {
            Err("unused".to_string())
        }
    }

    /// A session that does nothing: the slot gate only asks for its identity.
    struct TestSession {
        id: SessionId,
        events: broadcast::Sender<SessionEvent>,
    }

    #[async_trait::async_trait]
    impl WorkerSession for TestSession {
        fn id(&self) -> SessionId {
            self.id
        }

        fn events(&self) -> EventReceiver<SessionEvent> {
            EventReceiver::from_broadcast(self.events.subscribe())
        }

        async fn is_open(&self) -> bool {
            true
        }

        async fn send(&self, _command: SessionCommand) -> Result<(), String> {
            Ok(())
        }

        async fn snapshot(&self) -> Result<SessionSnapshot, String> {
            Err("unused".to_string())
        }

        async fn set_model(
            &self,
            _model: &str,
            _effort: Option<String>,
        ) -> Result<ModelSwitch, String> {
            Err("unused".to_string())
        }

        async fn close(&self) -> Result<(), String> {
            Ok(())
        }

        async fn close_and_wait(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn live_entry(agent: u64) -> LiveWorker {
        let (events, _) = broadcast::channel(8);
        let session: SharedWorkerSession = Arc::new(TestSession {
            id: serde_json::from_value(serde_json::json!(agent)).expect("session id"),
            events,
        });
        LiveWorker {
            session,
            busy: Arc::new(AtomicBool::new(false)),
            pump: tokio::spawn(std::future::pending::<()>()),
            cleanup_retry_only: false,
            strikes: Arc::new(Mutex::new(Strikes::new(3))),
            steer_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The catalog a real hub would offer: the team tests' scripted endpoint.
    fn test_catalog(dir: &Path) -> ModelCatalog {
        std::fs::write(
            dir.join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
        )
        .expect("config.toml");
        std::fs::write(
            dir.join("endpoints.toml"),
            "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted-a\"]\n",
        )
        .expect("endpoints.toml");
        let config = mnemo::config::Config::load(dir).expect("catalog config");
        ModelCatalog::from_config(&config, String::new())
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(root: &Path) {
        std::fs::create_dir_all(root).expect("repo dir");
        run_git(root, &["init", "-q", "-b", "wt/mnemo"]);
        run_git(root, &["config", "user.name", "Mnemo2 Test"]);
        run_git(root, &["config", "user.email", "mnemo2@example.invalid"]);
        std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
        std::fs::write(root.join(".gitignore"), ".worktrees/\n.coding/\n").expect("ignore file");
        run_git(root, &["add", "README.md", ".gitignore"]);
        run_git(root, &["commit", "-q", "-m", "seed"]);
    }

    /// A hub, a paired in-process node, and the team's remote seam over one
    /// scratch project. Everything lives under the checkout's `target/` (the
    /// repository's `.cargo/config.toml` points `TMP` there).
    struct Rig {
        root: tempfile::TempDir,
        _state: tempfile::TempDir,
        _hub_state: tempfile::TempDir,
        _config: tempfile::TempDir,
        _node_home: tempfile::TempDir,
        hub: Arc<HubServer>,
        workers: Arc<RemoteWorkers>,
        records: Arc<Mutex<Vec<WorkerRecord>>>,
        node_id: String,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Rig {
        async fn start() -> Self {
            let root = tempfile::tempdir().expect("project root");
            init_repo(root.path());
            let state = tempfile::tempdir().expect("team state");
            let hub_state = tempfile::tempdir().expect("hub state");
            let config = tempfile::tempdir().expect("hub config");
            let node_home = tempfile::tempdir().expect("node home");
            let catalog = test_catalog(config.path());
            let hub = Arc::new(
                HubServer::start(
                    HubOptions {
                        bind: "127.0.0.1:0".parse().expect("loopback"),
                        state_dir: hub_state.path().to_path_buf(),
                    },
                    Arc::new(StubServices { catalog }),
                )
                .expect("start the hub"),
            );
            let address = hub.local_addr().to_string();
            let code = hub.pairing().issue_code("node-a");
            let identity = node::pair(node_home.path(), &address, &code.code, "node-a")
                .await
                .expect("pair the node");
            let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let workspace = node_home.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("node workspace");
            let cfg = NodeConfig {
                home: node_home.path().to_path_buf(),
                workspace,
                outage_budget: mnemo::remote::HUB_OUTAGE_BUDGET,
                index_codegraph: false,
                slots: 2,
                extension: None,
            };
            tokio::spawn(node::run(cfg, async move {
                let _ = shutdown_rx.await;
            }));
            assert!(
                hub.wait_connected(&identity.node_id, Duration::from_secs(120))
                    .await,
                "the paired node connects"
            );
            let records = Arc::new(Mutex::new(Vec::new()));
            let live = Arc::new(Mutex::new(HashMap::new()));
            let workers = Arc::new(RemoteWorkers::new(
                root.path().to_path_buf(),
                state.path().to_path_buf(),
                records.clone(),
                live,
            ));
            workers.attach(Arc::new(TestHosts {
                server: hub.clone(),
                address,
            }));
            Self {
                root,
                _state: state,
                _hub_state: hub_state,
                _config: config,
                _node_home: node_home,
                hub,
                workers,
                records,
                node_id: identity.node_id,
                shutdown: Some(shutdown),
            }
        }

        async fn base(&self) -> String {
            git_value(self.root.path(), &["rev-parse", "HEAD"])
                .await
                .expect("base commit")
        }

        async fn synced_tip(&self, worker_id: &str) -> Option<String> {
            self.records
                .lock()
                .await
                .iter()
                .find(|record| record.id == worker_id)
                .and_then(|record| record.remote.as_ref())
                .and_then(|remote| remote.synced_tip.clone())
        }

        /// A team runtime over the same project, attached to `hosts`. The
        /// host is pre-measured so no qualification run starts in a test.
        async fn team_with(&self, hosts: Arc<dyn HostDirectory>) -> Arc<TeamRuntime> {
            let team = Arc::new(
                open_test_team(self.root.path(), self._state.path(), self._config.path()).await,
            );
            let commands = super::super::spawn::load_check_commands(self.root.path());
            team.remote
                .note_check(&self.node_id, &checks_hash(&commands), true, None);
            team.attach_hosts(hosts);
            team
        }

        /// A team runtime attached to the rig's own hub with the host NOT
        /// pre-measured: the first reconcile pass starts a qualification.
        async fn team_unmeasured(&self) -> Arc<TeamRuntime> {
            let team = Arc::new(
                open_test_team(self.root.path(), self._state.path(), self._config.path()).await,
            );
            team.attach_hosts(Arc::new(TestHosts {
                server: self.hub.clone(),
                address: self.hub.local_addr().to_string(),
            }));
            team
        }

        /// A team runtime attached to the rig's own hub.
        async fn team(&self) -> Arc<TeamRuntime> {
            self.team_with(Arc::new(TestHosts {
                server: self.hub.clone(),
                address: self.hub.local_addr().to_string(),
            }))
            .await
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    #[test]
    fn old_team_json_loads_without_remote() {
        let root = tempfile::tempdir().expect("root");
        let record = super::super::tests::worker(root.path(), "abcd1234");
        let mut value = serde_json::to_value(&record).expect("encode the record");
        value
            .as_object_mut()
            .expect("record object")
            .remove("remote");
        let loaded: WorkerRecord = serde_json::from_value(value).expect("an old ledger loads");
        assert!(loaded.remote.is_none());
        assert_eq!(loaded.host_label(), "local");
    }

    #[test]
    fn environmental_failure_classifies_not_found() {
        assert!(environmental_failure(Some(127), ""));
        assert!(environmental_failure(Some(9009), ""));
        assert!(environmental_failure(
            Some(1),
            "'pwsh' is not recognized as an internal or external command"
        ));
        assert!(environmental_failure(
            None,
            "bash: powershell: command not found"
        ));
        assert!(!environmental_failure(Some(101), "test failed: 3 passed, 1 failed"));
        assert!(!environmental_failure(None, "error[E0433]: cannot find crate"));
    }

    #[test]
    fn requires_windows_detects_powershell() {
        assert!(requires_windows(&["powershell -NoProfile -Command x".into()]));
        assert!(requires_windows(&["pwsh -File build.ps1".into()]));
        assert!(
            !requires_windows(&[r#"pwsh -Command "npm test""#.into()]),
            "pwsh alone runs on macOS and Linux too"
        );
        assert!(requires_windows(&["pwsh.exe -File build.ps1".into()]));
        assert!(
            requires_windows(&[r#"pwsh.exe -Command "npm test""#.into()]),
            "only a Windows config spells the executable pwsh.exe"
        );
        assert!(!requires_windows(&[r#""C:\Program Files\PowerShell\pwsh.exe" -Command x"#.into()]));
        assert!(requires_windows(&["cmd /c build.bat".into()]));
        assert!(requires_windows(&["node scripts/build.cmd".into()]));
        assert!(
            !requires_windows(&["bats tests/e2e.bats".into()]),
            "a .bats suite is not a Windows batch file"
        );
        assert!(!requires_windows(&["cargo test --workspace".into()]));
        assert!(!requires_windows(&["npm run check --workspace mnemo2".into()]));
        assert!(!requires_windows(&[]));
    }

    #[test]
    fn suitability_resets_when_checks_change() {
        let root = tempfile::tempdir().expect("root");
        let state = tempfile::tempdir().expect("state");
        let workers = RemoteWorkers::new(
            root.path().to_path_buf(),
            state.path().to_path_buf(),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(HashMap::new())),
        );
        let hash = checks_hash(&["cargo test --workspace".to_string()]);
        assert_eq!(workers.qualification("n1", &hash), Qualification::Pending);
        workers.note_check("n1", &hash, true, None);
        assert_eq!(workers.qualification("n1", &hash), Qualification::Passed);
        workers.note_check("n1", &hash, false, Some("`pwsh` could not run here".into()));
        assert_eq!(workers.qualification("n1", &hash), Qualification::Failed);
        let changed = checks_hash(&[
            "cargo test --workspace".to_string(),
            "cargo clippy --workspace --all-targets".to_string(),
        ]);
        assert_eq!(workers.qualification("n1", &changed), Qualification::Pending);
        // The record survives a restart of the app.
        let reloaded = HostSuitability::load(state.path());
        assert_eq!(
            reloaded.hosts.get("n1").map(|state| state.qualification),
            Some(Qualification::Failed)
        );
    }

    /// A team runtime over one scratch repo, for the slot gate and the host
    /// monitor tests.
    async fn open_test_team(root: &Path, state: &Path, config: &Path) -> TeamRuntime {
        let engine = mnemo::headless::Engine::start(mnemo::headless::EngineOptions {
            workspace: mnemo::workspace::Workspace::new(root),
            config_dir: Some(config.to_path_buf()),
            permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
            memory: mnemo::headless::MemoryBackend::Disabled,
            index_codegraph: false,
            require_git: true,
            auto_install_embedder: false,
        })
        .await
        .expect("start the test engine");
        TeamRuntime::open(TeamRuntimeOptions {
            engine: Arc::new(engine),
            workspace_root: root.to_path_buf(),
            state_dir: state.to_path_buf(),
            config_dir: config.to_path_buf(),
            classifier: None,
            routing_log_path: Some(state.join("dispatch_routing.jsonl")),
            slots: Some(2),
            models: Vec::new(),
            pricing: Vec::new(),
        })
        .await
        .expect("open the test team")
    }

    fn sample_remote(node_id: &str, synced: Option<&str>) -> RemoteCheckout {
        RemoteCheckout {
            node_id: node_id.to_string(),
            node_name: "node-a".to_string(),
            os: "windows".to_string(),
            project: "0123456789ab".to_string(),
            checkout: "0123456789ab-1".to_string(),
            path: "C:/node/checkouts/0123456789ab-1".to_string(),
            synced_tip: synced.map(str::to_string),
            offline_since_ms: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_workers_do_not_use_local_slots() {
        let root = tempfile::tempdir().expect("root");
        init_repo(root.path());
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        let _ = test_catalog(config.path());
        let team = Arc::new(open_test_team(root.path(), state.path(), config.path()).await);

        let mut remote_record = super::super::tests::worker(root.path(), "abcd1234");
        remote_record.remote = Some(sample_remote("n1", None));
        let local_record = super::super::tests::worker(root.path(), "ef567890");
        team.records.lock().await.push(remote_record.clone());
        team.records.lock().await.push(local_record.clone());
        team.live
            .lock()
            .await
            .insert(remote_record.id.clone(), live_entry(1));
        assert_eq!(
            team.active_workers_for_slots().await,
            0,
            "a remote worker uses its host's slots, not a local one"
        );
        team.live
            .lock()
            .await
            .insert(local_record.id.clone(), live_entry(2));
        assert_eq!(team.active_workers_for_slots().await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_past_budget_fails_resumably() {
        let root = tempfile::tempdir().expect("root");
        init_repo(root.path());
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        let catalog = test_catalog(config.path());
        let team = Arc::new(open_test_team(root.path(), state.path(), config.path()).await);
        // A hub whose node never connects: the worker's host is simply away.
        let hub_state = tempfile::tempdir().expect("hub state");
        let hub = Arc::new(
            HubServer::start(
                HubOptions {
                    bind: "127.0.0.1:0".parse().expect("loopback"),
                    state_dir: hub_state.path().to_path_buf(),
                },
                Arc::new(StubServices { catalog }),
            )
            .expect("start the hub"),
        );
        let address = hub.local_addr().to_string();
        team.attach_hosts(Arc::new(TestHosts { server: hub, address }));

        let mut record = super::super::tests::worker(root.path(), "abcd1234");
        // A worker that is still running: a finished one keeps its outcome.
        record.status = WorkerStatus::Working;
        record.base_commit = Some("1111111111111111111111111111111111111111".to_string());
        record.remote = Some(sample_remote(
            "deadbeefdeadbeefdeadbeefdeadbeef",
            Some("2222222222222222222222222222222222222222"),
        ));
        team.records.lock().await.push(record.clone());
        team.live
            .lock()
            .await
            .insert(record.id.clone(), live_entry(1));

        // First pass marks the host away; the second sees it past the budget.
        team.poll_hosts().await;
        {
            let mut records = team.records.lock().await;
            let stored = records
                .iter_mut()
                .find(|stored| stored.id == record.id)
                .expect("the worker record");
            stored
                .remote
                .as_mut()
                .expect("a remote worker")
                .offline_since_ms = Some(super::super::unix_ms().saturating_sub(200_000));
        }
        team.poll_hosts().await;

        let records = team.records.lock().await;
        let stored = records
            .iter()
            .find(|stored| stored.id == record.id)
            .expect("the worker record");
        assert_eq!(stored.status, WorkerStatus::Failed);
        let error = stored.error.clone().expect("a resumable error");
        assert!(
            error.contains("host 'node-a' has been offline for more than 120 s"),
            "{error}"
        );
        assert!(
            error.contains("last synced commit 2222222"),
            "{error}"
        );
        drop(records);
        assert!(
            !team.live.lock().await.contains_key(&record.id),
            "a lost host's session leaves the live set"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_session_events_are_demuxed_once_per_node() {
        let rig = Rig::start().await;
        let node = rig.hub.node(&rig.node_id);
        let first = rig
            .workers
            .demux(&node, "node-a", &rig.node_id)
            .await
            .expect("the first demux");
        let second = rig
            .workers
            .demux(&node, "node-a", &rig.node_id)
            .await
            .expect("a second demux");
        assert!(
            first.same_channel(&second),
            "one node has exactly one event demux"
        );
        assert_eq!(
            rig.workers
                .node_events
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sync_shadow_moves_the_shadow_branch_to_the_node_tip() {
        let rig = Rig::start().await;
        let base = rig.base().await;
        let remote = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-abcd1234", &base, false)
            .await
            .expect("prepare the node checkout");
        // The node runs on this machine in the test, so its checkout is a real
        // directory: commit in it as the worker would.
        let node_checkout = PathBuf::from(&remote.path);
        std::fs::write(node_checkout.join("work.txt"), "remote work\n").expect("write");
        run_git(&node_checkout, &["config", "user.name", "Mnemo2 Test"]);
        run_git(&node_checkout, &["config", "user.email", "mnemo2@example.invalid"]);
        run_git(&node_checkout, &["add", "-A"]);
        run_git(&node_checkout, &["commit", "-q", "-m", "remote work"]);
        let tip = git_value(&node_checkout, &["rev-parse", "HEAD"])
            .await
            .expect("the node tip");
        assert_ne!(tip, base);

        // The shadow is an ordinary pool checkout of the hub repo.
        let shadow = rig.root.path().join(".worktrees").join("agent-abcd1234");
        run_git(
            rig.root.path(),
            &[
                "worktree",
                "add",
                "-q",
                shadow.to_str().expect("utf-8 path"),
                "-b",
                "wt/agent-abcd1234",
                &base,
            ],
        );
        let mut record = super::super::tests::worker(rig.root.path(), "abcd1234");
        record.branch = "wt/agent-abcd1234".to_string();
        record.worktree = shadow.clone();
        record.base_commit = Some(base);
        record.remote = Some(remote.clone());
        rig.records.lock().await.push(record.clone());

        let sync = rig
            .workers
            .sync_shadow(&record, true)
            .await
            .expect("sync the shadow");
        assert!(sync.changed);
        assert_eq!(sync.tip, tip);
        assert!(sync.dirty.is_empty(), "the node tree is clean");
        assert_eq!(
            crate::mnemo2::phase3::tip(&shadow).await.expect("shadow tip"),
            tip
        );
        assert_eq!(rig.synced_tip("abcd1234").await, Some(tip));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sync_shadow_refuses_a_dirty_shadow() {
        let rig = Rig::start().await;
        let base = rig.base().await;
        let remote = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-abcd1234", &base, false)
            .await
            .expect("prepare the node checkout");
        let shadow = rig.root.path().join(".worktrees").join("agent-abcd1234");
        run_git(
            rig.root.path(),
            &[
                "worktree",
                "add",
                "-q",
                shadow.to_str().expect("utf-8 path"),
                "-b",
                "wt/agent-abcd1234",
                &base,
            ],
        );
        let mut record = super::super::tests::worker(rig.root.path(), "abcd1234");
        record.branch = "wt/agent-abcd1234".to_string();
        record.worktree = shadow.clone();
        record.base_commit = Some(base.clone());
        record.remote = Some(remote.clone());
        rig.records.lock().await.push(record.clone());
        rig.workers
            .sync_shadow(&record, true)
            .await
            .expect("the first sync");

        // Local work in the hub copy, and a new commit on the node.
        std::fs::write(shadow.join("local-only.txt"), "mine\n").expect("local work");
        let node_checkout = PathBuf::from(&remote.path);
        std::fs::write(node_checkout.join("more.txt"), "more\n").expect("write");
        run_git(&node_checkout, &["config", "user.name", "Mnemo2 Test"]);
        run_git(&node_checkout, &["config", "user.email", "mnemo2@example.invalid"]);
        run_git(&node_checkout, &["add", "-A"]);
        run_git(&node_checkout, &["commit", "-q", "-m", "more"]);

        let error = rig
            .workers
            .sync_shadow(&record, true)
            .await
            .expect_err("a dirty hub copy is never overwritten");
        assert!(error.contains("has local changes; Mnemo will not overwrite it"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sync_shadow_offline_names_the_host() {
        let mut rig = Rig::start().await;
        let base = rig.base().await;
        let record_remote = sample_remote(&rig.node_id, None);
        if let Some(shutdown) = rig.shutdown.take() {
            let _ = shutdown.send(());
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        while rig.hub.is_connected(&rig.node_id) {
            assert!(Instant::now() < deadline, "the node link drops");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut record = super::super::tests::worker(rig.root.path(), "abcd1234");
        record.base_commit = Some(base);
        record.remote = Some(record_remote);
        let error = rig
            .workers
            .sync_shadow(&record, true)
            .await
            .expect_err("an offline host cannot answer");
        assert!(
            error.contains("host 'node-a' is offline; Mnemo cannot read worker 'abcd1234's latest commits"),
            "{error}"
        );
    }

    async fn checkout_list(rig: &Rig) -> Vec<mnemo::remote::repo::CheckoutInfo> {
        let value = rig
            .hub
            .node(&rig.node_id)
            .call(
                mnemo::remote::repo::CHECKOUT_LIST,
                json!(mnemo::remote::repo::CheckoutList {
                    project: rig.workers.project().to_string(),
                }),
                Duration::from_secs(120),
            )
            .await
            .expect("the node lists its checkouts");
        serde_json::from_value::<mnemo::remote::repo::CheckoutListAnswer>(value)
            .expect("a checkout list answer")
            .checkouts
    }

    async fn sessions(rig: &Rig) -> Vec<mnemo::remote::hub::RemoteSessionInfo> {
        rig.hub
            .node(&rig.node_id)
            .list_sessions()
            .await
            .expect("the node lists its sessions")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sync_shadow_handles_an_unchanged_answer() {
        let rig = Rig::start().await;
        let base = rig.base().await;
        let remote = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-abcd1234", &base, false)
            .await
            .expect("prepare the node checkout");
        let node_checkout = PathBuf::from(&remote.path);
        std::fs::write(node_checkout.join("work.txt"), "remote work\n").expect("write");
        run_git(&node_checkout, &["config", "user.name", "Mnemo2 Test"]);
        run_git(&node_checkout, &["config", "user.email", "mnemo2@example.invalid"]);
        run_git(&node_checkout, &["add", "-A"]);
        run_git(&node_checkout, &["commit", "-q", "-m", "remote work"]);
        let tip = git_value(&node_checkout, &["rev-parse", "HEAD"])
            .await
            .expect("the node tip");

        let shadow = rig.root.path().join(".worktrees").join("agent-abcd1234");
        run_git(
            rig.root.path(),
            &[
                "worktree",
                "add",
                "-q",
                shadow.to_str().expect("utf-8 path"),
                "-b",
                "wt/agent-abcd1234",
                &base,
            ],
        );
        let mut record = super::super::tests::worker(rig.root.path(), "abcd1234");
        record.branch = "wt/agent-abcd1234".to_string();
        record.worktree = shadow.clone();
        record.base_commit = Some(base.clone());
        record.remote = Some(remote);
        rig.records.lock().await.push(record.clone());

        // First sync carries the node tip into the hub.
        rig.workers
            .sync_shadow(&record, true)
            .await
            .expect("the first sync fetches the tip");
        // Now the hub has the tip as the lead branch's head while the shadow
        // sits back at the base commit, and the remote ref is gone: the node
        // answers `unchanged`, so the second sync must not fetch anything.
        run_git(&shadow, &["reset", "--hard", &base]);
        run_git(rig.root.path(), &["update-ref", "refs/heads/wt/mnemo", &tip]);
        run_git(
            rig.root.path(),
            &["update-ref", "-d", "refs/mnemo/remote/abcd1234"],
        );

        let sync = rig
            .workers
            .sync_shadow(&record, true)
            .await
            .expect("an unchanged answer still moves the shadow");
        assert!(sync.changed);
        assert_eq!(sync.tip, tip);
        assert_eq!(
            crate::mnemo2::phase3::tip(&shadow).await.expect("shadow tip"),
            tip
        );
        let refetched = std::process::Command::new("git")
            .arg("-C")
            .arg(rig.root.path())
            .args(["rev-parse", "--verify", "refs/mnemo/remote/abcd1234"])
            .output()
            .expect("probe the remote ref");
        assert!(
            !refetched.status.success(),
            "an unchanged answer carries no bundle"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reconcile_releases_only_unreferenced_checkouts() {
        let rig = Rig::start().await;
        let base = rig.base().await;
        let referenced = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-aaaa1111", &base, false)
            .await
            .expect("the referenced checkout");
        let orphan = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-bbbb2222", &base, false)
            .await
            .expect("the unreferenced checkout");
        assert_ne!(referenced.checkout, orphan.checkout);
        let team = rig.team().await;
        let mut record = super::super::tests::worker(rig.root.path(), "aaaa1111");
        record.remote = Some(referenced.clone());
        team.records.lock().await.push(record);

        team.reconcile_hosts().await;

        let checkouts = checkout_list(&rig).await;
        let kept = checkouts
            .iter()
            .find(|info| info.checkout == referenced.checkout)
            .expect("the referenced checkout is kept");
        assert_eq!(kept.branch.as_deref(), Some("wt/agent-aaaa1111"));
        let released = checkouts
            .iter()
            .find(|info| info.checkout == orphan.checkout)
            .expect("the pool is kept, detached");
        assert!(
            released.branch.is_none(),
            "the unreferenced checkout is released"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reconcile_keeps_a_checkout_a_dispatch_is_preparing() {
        let rig = Rig::start().await;
        let base = rig.base().await;
        let preparing = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-cccc3333", &base, false)
            .await
            .expect("the checkout being prepared");
        let team = rig.team().await;
        team.remote.reserve(&rig.node_id, "wt/agent-cccc3333");

        team.reconcile_hosts().await;
        let kept = checkout_list(&rig)
            .await
            .into_iter()
            .find(|info| info.checkout == preparing.checkout)
            .expect("the checkout is listed");
        assert_eq!(
            kept.branch.as_deref(),
            Some("wt/agent-cccc3333"),
            "a reserved checkout is not an orphan"
        );

        team.remote.release_reservation(&rig.node_id, "wt/agent-cccc3333");
        team.reconcile_hosts().await;
        let released = checkout_list(&rig)
            .await
            .into_iter()
            .find(|info| info.checkout == preparing.checkout)
            .expect("the checkout is listed");
        assert!(
            released.branch.is_none(),
            "once released it is an orphan like any other"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reconcile_keeps_a_running_qualification_checkout() {
        let rig = Rig::start().await;
        // A check that takes several seconds, so the qualification is still
        // running while the reconcile pass below looks at its checkout.
        let slow = if std::env::consts::OS == "windows" {
            "ping -n 8 127.0.0.1"
        } else {
            "ping -c 8 127.0.0.1"
        };
        std::fs::create_dir_all(rig.root.path().join(".coding")).expect("coding dir");
        std::fs::write(
            rig.root.path().join(".coding/checks.toml"),
            format!("commands = [\"{slow}\"]\n"),
        )
        .expect("checks file");
        let team = rig.team_unmeasured().await;
        // The monitor's first pass starts the qualification; wait until its
        // hostcheck checkout is prepared.
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let held = checkout_list(&rig)
                .await
                .iter()
                .any(|info| info.branch.as_deref() == Some(QUALIFY_BRANCH));
            if held {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the qualification prepares its hostcheck checkout"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            team.remote.qualifying_checkout(&rig.node_id).is_some(),
            "the qualification is still running"
        );

        team.reconcile_hosts().await;

        assert!(
            checkout_list(&rig)
                .await
                .iter()
                .any(|info| info.branch.as_deref() == Some(QUALIFY_BRANCH)),
            "a reconcile pass must not release the checkout a qualification is using"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reconcile_closes_an_orphan_only_on_the_second_pass() {
        let rig = Rig::start().await;
        let base = rig.base().await;
        let remote = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-cccc3333", &base, false)
            .await
            .expect("the orphan's checkout");
        let session = rig
            .hub
            .node(&rig.node_id)
            .spawn_session(RemoteSpawn {
                options: SpawnOptions::default(),
                checkout: Some(remote.checkout.clone()),
            })
            .await
            .expect("a session the hub does not track");
        let team = rig.team().await;
        // Keep the checkout alive so only the session rule is under test.
        let mut record = super::super::tests::worker(rig.root.path(), "cccc3333");
        record.remote = Some(remote.clone());
        team.records.lock().await.push(record);

        team.reconcile_hosts().await;
        assert!(
            sessions(&rig).await.iter().any(|info| info.session == session),
            "a session seen for the first time may still be spawning"
        );
        team.reconcile_hosts().await;
        assert!(
            !sessions(&rig).await.iter().any(|info| info.session == session),
            "an orphan is closed on the next pass"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn monitor_starts_when_the_hub_appears_after_attach() {
        let rig = Rig::start().await;
        let base = rig.base().await;
        let orphan = rig
            .workers
            .prepare_checkout(&rig.node_id, "wt/agent-dddd4444", &base, false)
            .await
            .expect("the unreferenced checkout");
        let late = Arc::new(LateHosts {
            server: StdMutex::new(None),
            address: rig.hub.local_addr().to_string(),
        });
        let _team = rig.team_with(late.clone()).await;
        // No hub yet: nothing may be reconciled.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            checkout_list(&rig)
                .await
                .iter()
                .any(|info| info.checkout == orphan.checkout && info.branch.is_some()),
            "without a hub the monitor leaves the node alone"
        );
        // The hub appears; the already-running monitor must pick it up.
        *late
            .server
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(rig.hub.clone());
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let released = checkout_list(&rig)
                .await
                .iter()
                .any(|info| info.checkout == orphan.checkout && info.branch.is_none());
            if released {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the monitor reconciles once the hub appears"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restarted_host_frees_the_slot() {
        let rig = Rig::start().await;
        let team = rig.team().await;
        let mut record = super::super::tests::worker(rig.root.path(), "abcd1234");
        // A worker that is still running: a finished one keeps its outcome.
        record.status = WorkerStatus::Working;
        let mut remote = sample_remote(&rig.node_id, None);
        remote.offline_since_ms = Some(super::super::unix_ms().saturating_sub(5_000));
        record.remote = Some(remote);
        team.records.lock().await.push(record.clone());
        team.live
            .lock()
            .await
            .insert(record.id.clone(), live_entry(7));
        assert_eq!(team.remote.busy_on(&rig.node_id).await, 1);

        // The host is back, but its node process no longer has the session.
        team.poll_hosts().await;

        assert_eq!(
            team.remote.busy_on(&rig.node_id).await,
            0,
            "a restarted host frees its slot"
        );
        let records = team.records.lock().await;
        let stored = records
            .iter()
            .find(|stored| stored.id == record.id)
            .expect("the worker record");
        assert_eq!(stored.status, WorkerStatus::Interrupted);
        assert!(
            stored
                .error
                .as_deref()
                .unwrap_or("")
                .contains("restarted; its session ended"),
            "{:?}",
            stored.error
        );
        assert!(stored
            .remote
            .as_ref()
            .expect("a remote worker")
            .offline_since_ms
            .is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_finished_worker_keeps_its_outcome_when_its_host_restarts() {
        let rig = Rig::start().await;
        let team = rig.team().await;
        let statuses = [
            WorkerStatus::Completed,
            WorkerStatus::InReview,
            WorkerStatus::Merged,
            WorkerStatus::Failed,
            WorkerStatus::Stopped,
            WorkerStatus::Interrupted,
            WorkerStatus::Idle,
        ];
        for (index, status) in statuses.into_iter().enumerate() {
            for lost in [false, true] {
                let id = format!("abcd{index}{}", u8::from(lost));
                let mut record = super::super::tests::worker(rig.root.path(), &id);
                record.status = status;
                record.error = Some("its own error".to_string());
                let mut remote = sample_remote(&rig.node_id, None);
                remote.offline_since_ms = Some(super::super::unix_ms().saturating_sub(5_000));
                record.remote = Some(remote.clone());
                team.records.lock().await.push(record.clone());
                team.live
                    .lock()
                    .await
                    .insert(record.id.clone(), live_entry(100 + index as u64));
                assert!(team.remote.busy_on(&rig.node_id).await >= 1);

                if lost {
                    team.fail_lost_host(&id, &remote).await;
                } else {
                    team.mark_host_restarted(&id, &remote).await;
                }

                let records = team.records.lock().await;
                let stored = records
                    .iter()
                    .find(|stored| stored.id == id)
                    .expect("the worker record");
                assert_eq!(stored.status, status, "{status:?} keeps its status");
                assert_eq!(
                    stored.error.as_deref(),
                    Some("its own error"),
                    "{status:?} keeps its error"
                );
                drop(records);
                assert!(
                    !team.live.lock().await.contains_key(&id),
                    "{status:?}: the lost session is dropped"
                );
                assert_eq!(
                    team.remote.busy_on(&rig.node_id).await,
                    0,
                    "{status:?}: the host slot is free"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_local_refuses_a_worker_still_running_on_its_host() {
        let rig = Rig::start().await;
        let team = rig.team().await;
        let mut record = super::super::tests::worker(rig.root.path(), "abcd1111");
        record.status = WorkerStatus::Idle;
        std::fs::create_dir_all(&record.worktree).expect("worktree");
        record.remote = Some(sample_remote(&rig.node_id, None));
        team.records.lock().await.push(record);
        team.live
            .lock()
            .await
            .insert("abcd1111".to_string(), live_entry(31));

        let answer = team
            .handle_lead_host_tool(
                "team_resume",
                serde_json::json!({"worker_id": "abcd1111", "host": "local"}),
            )
            .await;
        assert!(!answer.success, "{}", answer.output);
        assert_eq!(
            answer.output,
            "worker 'abcd1111' is still running on host 'node-a'; stop it first, then resume it with host \"local\""
        );
        let records = team.records.lock().await;
        assert!(
            records[0].remote.is_some(),
            "a refused resume leaves the worker on its host"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_local_with_no_free_local_slot_leaves_the_worker_on_its_host() {
        let rig = Rig::start().await;
        let team = rig.team().await;
        let mut record = super::super::tests::worker(rig.root.path(), "abcd3333");
        record.status = WorkerStatus::Interrupted;
        std::fs::create_dir_all(&record.worktree).expect("worktree");
        record.remote = Some(sample_remote(&"f".repeat(32), None));
        team.records.lock().await.push(record);
        // No local slot is free.
        team.persistence
            .slots
            .store(0, std::sync::atomic::Ordering::SeqCst);

        let answer = team
            .handle_lead_host_tool(
                "team_resume",
                serde_json::json!({"worker_id": "abcd3333", "host": "local"}),
            )
            .await;
        assert!(!answer.success, "{}", answer.output);
        assert_eq!(answer.output, "all 0 worker slots are busy");
        let records = team.records.lock().await;
        assert!(
            records[0].remote.is_some(),
            "a refused resume leaves the record on its host"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_names_a_host_that_is_no_longer_paired() {
        let rig = Rig::start().await;
        let team = rig.team().await;
        let mut record = super::super::tests::worker(rig.root.path(), "abcd2222");
        record.status = WorkerStatus::Interrupted;
        std::fs::create_dir_all(&record.worktree).expect("worktree");
        let mut remote = sample_remote(&"f".repeat(32), None);
        remote.node_name = "ghost".to_string();
        record.remote = Some(remote);
        team.records.lock().await.push(record);

        let answer = team
            .handle_lead_host_tool("team_resume", serde_json::json!({"worker_id": "abcd2222"}))
            .await;
        assert!(!answer.success, "{}", answer.output);
        assert_eq!(
            answer.output,
            "host 'ghost' is no longer paired; pass host \"local\" to continue worker 'abcd2222' here"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_host_fails_only_its_running_workers() {
        let rig = Rig::start().await;
        let team = rig.team().await;
        for (id, status) in [
            ("abcd1111", WorkerStatus::Working),
            ("abcd2222", WorkerStatus::Completed),
        ] {
            let mut record = super::super::tests::worker(rig.root.path(), id);
            record.status = status;
            record.base_commit = Some("1111111111111111111111111111111111111111".to_string());
            record.remote = Some(sample_remote(&rig.node_id, None));
            team.records.lock().await.push(record);
            team.live
                .lock()
                .await
                .insert(id.to_string(), live_entry(if id == "abcd1111" { 21 } else { 22 }));
        }

        let answer = team
            .handle_lead_host_tool(
                "team_hosts",
                serde_json::json!({"action": "revoke", "name": "node-a"}),
            )
            .await;
        assert!(answer.success, "{}", answer.output);
        let answer: serde_json::Value = serde_json::from_str(&answer.output).expect("JSON");
        assert_eq!(answer["failed_workers"], serde_json::json!(["abcd1111"]));

        let records = team.records.lock().await;
        let running = records.iter().find(|w| w.id == "abcd1111").expect("running");
        assert_eq!(running.status, WorkerStatus::Failed);
        assert!(
            running.error.as_deref().unwrap_or("").contains("was revoked"),
            "{:?}",
            running.error
        );
        assert!(
            running
                .error
                .as_deref()
                .unwrap_or("")
                .contains("from its starting commit 1111111 (nothing was synced from node-a)"),
            "{:?}",
            running.error
        );
        let finished = records.iter().find(|w| w.id == "abcd2222").expect("finished");
        assert_eq!(finished.status, WorkerStatus::Completed);
        drop(records);
        assert!(
            team.live.lock().await.is_empty(),
            "both lost sessions are dropped"
        );
    }
}
