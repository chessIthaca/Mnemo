// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The node's git side: one bare repo per project, warm pool checkouts, bundle
//! transfer in both directions, and the project's checks (plan 9a2f4b67,
//! phase 3 step 3).
//!
//! The hub never names a node path. It names a *project* (a 12-hex key), a
//! *checkout* (one path component under `<home>/checkouts/`) and a *branch*;
//! code travels as git bundles, pushed up and pulled back in pieces of
//! [`BUNDLE_CHUNK`] raw bytes each, base64-encoded inside the link's existing
//! frame budget.
//!
//! Layout under the node's home:
//!
//! ```text
//! <home>/repos/<project>.git        one bare repo per project
//! <home>/checkouts/<project>-<n>    the pool checkouts the hub drives
//! <home>/work/<upload>.bundle       an in-flight push, dropped after UPLOAD_IDLE
//! <home>/tmp/<uuid>.bundle          a one-shot outgoing bundle
//! ```
//!
//! Every checkout is a `git worktree` of the project's bare repo, so all pools
//! of a project share one object store. A release runs `clean -fd` but never
//! `-x`, so the ignored `target/` folder inside a pool stays warm for the next
//! worker — which is the whole reason the pools exist.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::remote::link::StreamTx;
use crate::remote::node::{checkout_path, CHECKOUTS_DIR};
use crate::remote::RemoteError;

/// `repo.have`: which of the hub's commits and ref tips this node already has.
pub const REPO_HAVE: &str = "repo.have";
/// `repo.push_begin`: open one bundle upload.
pub const REPO_PUSH_BEGIN: &str = "repo.push_begin";
/// `repo.push_chunk`: append one base64 piece to an open upload.
pub const REPO_PUSH_CHUNK: &str = "repo.push_chunk";
/// `repo.push_end`: verify an upload and fetch its refs into the bare repo.
pub const REPO_PUSH_END: &str = "repo.push_end";
/// `checkout.prepare`: hand out a pool checkout on a worker branch.
pub const CHECKOUT_PREPARE: &str = "checkout.prepare";
/// `checkout.list`: the pools this project currently has.
pub const CHECKOUT_LIST: &str = "checkout.list";
/// `checkout.release`: detach a pool, drop its branch, trim the extras.
pub const CHECKOUT_RELEASE: &str = "checkout.release";
/// `repo.tip`: one checkout's HEAD, branch and dirty paths.
pub const REPO_TIP: &str = "repo.tip";
/// `repo.fetch_branch`: stream a checkout's branch back as a bundle.
pub const REPO_FETCH_BRANCH: &str = "repo.fetch_branch";
/// `checks.run`: run the project's check commands in one checkout.
pub const CHECKS_RUN: &str = "checks.run";

/// Raw bytes per streamed bundle piece. Base64 inflates this to ~5.6 MiB,
/// comfortably below the link's 16 MiB frame ceiling, so `MAX_MESSAGE` never
/// has to move.
pub const BUNDLE_CHUNK: usize = 4 * 1024 * 1024;
/// The directory holding one bare repo per project.
pub const REPOS_DIR: &str = "repos";
/// The directory holding one-shot outgoing bundles.
pub const TMP_DIR: &str = "tmp";
/// The directory holding in-flight uploads.
pub const WORK_DIR: &str = "work";
/// How many pool checkouts a node keeps warm per project unless told otherwise.
pub const DEFAULT_NODE_SLOTS: usize = 2;
/// How long an upload may sit untouched before the next `repo.push_begin`
/// sweeps it away.
pub const UPLOAD_IDLE: Duration = Duration::from_secs(600);

/// `repo.have`'s question: which of `want` this node already has.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HaveRequest {
    /// The 12-hex project key.
    pub project: String,
    /// Commit shas the hub would rather not send again.
    #[serde(default)]
    pub want: Vec<String>,
}

/// `repo.have`'s answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HaveAnswer {
    /// The requested shas this node does not have.
    pub missing: Vec<String>,
    /// Up to 256 ref tips, newest committer date first.
    pub tips: Vec<String>,
}

/// `repo.push_begin`: one bundle about to arrive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushBegin {
    /// The 12-hex project key.
    pub project: String,
    /// The bundle's size in bytes.
    pub size: u64,
    /// The bundle's `sha256`, hex.
    pub sha256: String,
}

/// `repo.push_begin`'s answer: the upload id the pieces name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushStarted {
    /// The upload id.
    pub upload: String,
}

/// `repo.push_chunk`: one piece of a bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushChunk {
    /// The upload id `repo.push_begin` answered.
    pub upload: String,
    /// The byte offset this piece starts at (must equal the bytes received).
    pub offset: u64,
    /// The piece, base64.
    pub data: String,
}

/// `repo.push_chunk`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushProgress {
    /// Bytes received so far.
    pub received: u64,
}

/// `repo.push_end`: the upload is complete; these refs should now exist.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushEnd {
    /// The upload id.
    pub upload: String,
    /// The refs the bundle carries, with the sha each must land on.
    #[serde(default)]
    pub refs: Vec<BundleRef>,
}

/// One ref a bundle carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRef {
    /// The full ref name, e.g. `refs/heads/wt/agent-fix`.
    pub name: String,
    /// The commit sha that ref must resolve to.
    pub sha: String,
}

/// `checkout.prepare`: a pool checkout on `branch`, based on `base`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutPrepare {
    /// The 12-hex project key.
    pub project: String,
    /// The worker branch the checkout is put on.
    pub branch: String,
    /// The commit the branch starts at.
    pub base: String,
    /// Reuse the checkout already on `branch` untouched, if there is one.
    #[serde(default)]
    pub keep_existing: bool,
}

/// `checkout.prepare`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutAnswer {
    /// The checkout name (one path component under `<home>/checkouts/`).
    pub checkout: String,
    /// The checkout's path on the node.
    pub path: String,
    /// The checkout's `HEAD` after preparation.
    pub tip: String,
    /// Whether this was the pool already on `branch` (`keep_existing`).
    pub reused: bool,
}

/// `checkout.list`: the pools of one project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutList {
    /// The 12-hex project key.
    pub project: String,
}

/// One pool checkout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutInfo {
    /// The checkout name.
    pub checkout: String,
    /// The checkout's path on the node.
    pub path: String,
    /// The branch `HEAD` is on, `None` while detached.
    pub branch: Option<String>,
    /// `HEAD`'s commit sha.
    pub tip: String,
    /// Whether the working tree has uncommitted changes.
    pub dirty: bool,
}

/// `checkout.list`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutListAnswer {
    /// Every pool of the project, lowest index first.
    pub checkouts: Vec<CheckoutInfo>,
}

/// `checkout.release`: the hub is done with one pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutRelease {
    /// The checkout name.
    pub checkout: String,
}

/// `repo.tip`: what one checkout currently holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoTip {
    /// The checkout name.
    pub checkout: String,
}

/// `repo.tip`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TipAnswer {
    /// `HEAD`'s commit sha.
    pub tip: String,
    /// The branch `HEAD` is on, `None` while detached.
    pub branch: Option<String>,
    /// Up to 50 `git status --porcelain` lines.
    pub dirty: Vec<String>,
}

/// `repo.fetch_branch`: stream one checkout's branch back to the hub.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchBranch {
    /// The checkout name.
    pub checkout: String,
    /// The branch to bundle.
    pub branch: String,
    /// Commits the hub already has; the bundle excludes their history.
    #[serde(default)]
    pub haves: Vec<String>,
}

/// The `start` value of a `repo.fetch_branch` stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchStart {
    /// The branch's tip on the node.
    pub tip: String,
    /// Up to 50 `git status --porcelain` lines.
    pub dirty: Vec<String>,
    /// The bundle's size in bytes (`0` when `unchanged`).
    pub size: u64,
    /// The bundle's `sha256`, hex (`""` when `unchanged`).
    pub sha256: String,
    /// Whether the hub already has everything (no pieces follow).
    pub unchanged: bool,
}

/// `checks.run`: run the project's checks in one checkout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecksRun {
    /// The checkout name.
    pub checkout: String,
    /// The check commands, exactly as `.coding/checks.toml` lists them.
    pub commands: Vec<String>,
    /// The tip the hub believes it is checking.
    pub tip: String,
}

/// `checks.run`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecksAnswer {
    /// `HEAD` before the run.
    pub tip_before: String,
    /// `HEAD` after the run.
    pub tip_after: String,
    /// The `run_checks` tool's result, verbatim.
    pub result: crate::tool::ToolResult,
}

/// One in-flight bundle upload.
struct Upload {
    /// The project the bundle belongs to.
    project: String,
    /// Where the bytes are landing.
    path: PathBuf,
    /// The size `repo.push_begin` promised.
    size: u64,
    /// The `sha256` `repo.push_begin` promised, hex.
    sha256: String,
    /// Bytes received so far; the next piece's offset.
    received: u64,
    /// When the last piece arrived, for the idle sweep.
    last: Instant,
}

/// A prepared `repo.fetch_branch` answer: the start value plus, when there is
/// new code, the bundle file still to be streamed and deleted.
struct BranchBundle {
    /// The stream's `start` value.
    start: FetchStart,
    /// The bundle file, `None` when the hub already has everything.
    file: Option<PathBuf>,
}

/// The node's repo, checkout and check surface — one per node process.
///
/// Every method is one entry of the node's `repo.*` / `checkout.*` /
/// `checks.run` protocol; [`RepoHost::handle`] dispatches them and answers
/// `None` for a method that belongs to some other handler.
pub struct RepoHost {
    /// The node's home (see the module docs for the layout).
    home: PathBuf,
    /// How many pool checkouts per project stay warm.
    slots: usize,
    /// In-flight uploads, by upload id.
    uploads: Mutex<HashMap<String, Upload>>,
    /// One lock per checkout (and per project while a pool is being picked), so
    /// two calls never drive the same worktree at once.
    checkout_locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl RepoHost {
    /// A repo host over `home`, keeping `slots` pool checkouts per project.
    pub fn new(home: impl Into<PathBuf>, slots: usize) -> Self {
        let home = home.into();
        for dir in [REPOS_DIR, CHECKOUTS_DIR, TMP_DIR, WORK_DIR] {
            let _ = std::fs::create_dir_all(home.join(dir));
        }
        Self {
            home,
            slots: slots.max(1),
            uploads: Mutex::new(HashMap::new()),
            checkout_locks: StdMutex::new(HashMap::new()),
        }
    }

    /// The node home this host works in.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// How many pool checkouts per project stay warm.
    pub fn slots(&self) -> usize {
        self.slots
    }

    /// Answer one `repo.*` / `checkout.*` / `checks.run` call, or `None` when
    /// the method belongs to some other handler.
    pub async fn handle(
        &self,
        method: &str,
        params: Value,
        out: StreamTx,
    ) -> Option<Result<Value, RemoteError>> {
        let answer = match method {
            REPO_HAVE => self.have(params).await,
            REPO_PUSH_BEGIN => self.push_begin(params).await,
            REPO_PUSH_CHUNK => self.push_chunk(params).await,
            REPO_PUSH_END => self.push_end(params).await,
            CHECKOUT_PREPARE => self.prepare(params).await,
            CHECKOUT_LIST => self.list_checkouts(params).await,
            CHECKOUT_RELEASE => self.release(params).await,
            REPO_TIP => self.tip(params).await,
            REPO_FETCH_BRANCH => self.fetch_branch(params, out).await,
            CHECKS_RUN => self.run_checks(params).await,
            _ => return None,
        };
        Some(answer)
    }

    /// `repo.have`: the requested shas this node lacks, plus its newest tips.
    async fn have(&self, params: Value) -> Result<Value, RemoteError> {
        let request: HaveRequest = parse(params, REPO_HAVE)?;
        project_key(&request.project)?;
        for sha in &request.want {
            commit_sha(sha)?;
        }
        let repo = self.repo_dir(&request.project);
        if !repo.is_dir() {
            return Ok(json!(HaveAnswer {
                missing: request.want,
                tips: Vec::new(),
            }));
        }
        let mut missing = Vec::new();
        for sha in &request.want {
            let probe = format!("{sha}^{{commit}}");
            if self
                .bare(&repo, &["cat-file", "-e", &probe])
                .await
                .is_err()
            {
                missing.push(sha.clone());
            }
        }
        let listing = self
            .bare(
                &repo,
                &[
                    "for-each-ref",
                    "--sort=-committerdate",
                    "--format=%(objectname)",
                    "refs/heads/",
                ],
            )
            .await?;
        let tips = listing
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(256)
            .map(str::to_string)
            .collect();
        Ok(json!(HaveAnswer { missing, tips }))
    }

    /// `repo.push_begin`: open an upload for one bundle.
    async fn push_begin(&self, params: Value) -> Result<Value, RemoteError> {
        let request: PushBegin = parse(params, REPO_PUSH_BEGIN)?;
        project_key(&request.project)?;
        hex_digest(&request.sha256)?;
        self.sweep_uploads().await;
        let upload = uuid::Uuid::new_v4().simple().to_string();
        let path = self.home.join(WORK_DIR).join(format!("{upload}.bundle"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RemoteError::internal(format!("{REPO_PUSH_BEGIN}: {error}")))?;
        }
        std::fs::File::create(&path)
            .map_err(|error| RemoteError::internal(format!("{REPO_PUSH_BEGIN}: {error}")))?;
        self.uploads.lock().await.insert(
            upload.clone(),
            Upload {
                project: request.project,
                path,
                size: request.size,
                sha256: request.sha256,
                received: 0,
                last: Instant::now(),
            },
        );
        Ok(json!(PushStarted { upload }))
    }

    /// `repo.push_chunk`: append one base64 piece at its offset.
    async fn push_chunk(&self, params: Value) -> Result<Value, RemoteError> {
        let request: PushChunk = parse(params, REPO_PUSH_CHUNK)?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(request.data.as_bytes())
            .map_err(|error| {
                RemoteError::bad_params(format!(
                    "{REPO_PUSH_CHUNK}: the piece is not base64: {error}"
                ))
            })?;
        let mut uploads = self.uploads.lock().await;
        let upload = uploads.get_mut(&request.upload).ok_or_else(|| {
            RemoteError::bad_params(format!(
                "{REPO_PUSH_CHUNK}: no upload '{}' is open",
                request.upload
            ))
        })?;
        if request.offset != upload.received {
            return Err(RemoteError::bad_params(format!(
                "{REPO_PUSH_CHUNK}: the piece starts at {}, the upload expects {}",
                request.offset, upload.received
            )));
        }
        let path = upload.path.clone();
        write_at(&path, request.offset, &data).await?;
        upload.received += data.len() as u64;
        upload.last = Instant::now();
        Ok(json!(PushProgress {
            received: upload.received
        }))
    }

    /// `repo.push_end`: verify the upload, then fetch its refs.
    async fn push_end(&self, params: Value) -> Result<Value, RemoteError> {
        let request: PushEnd = parse(params, REPO_PUSH_END)?;
        let upload = self
            .uploads
            .lock()
            .await
            .remove(&request.upload)
            .ok_or_else(|| {
                RemoteError::bad_params(format!(
                    "{REPO_PUSH_END}: no upload '{}' is open",
                    request.upload
                ))
            })?;
        for reference in &request.refs {
            self.check_ref(&reference.name).await?;
            commit_sha(&reference.sha)?;
        }
        let verified = verify_file(&upload.path, upload.size, &upload.sha256).await;
        let result = match verified {
            Ok(()) => {
                self.land_bundle(&upload.project, &upload.path, &request.refs)
                    .await
            }
            Err(error) => Err(error),
        };
        let _ = std::fs::remove_file(&upload.path);
        result
    }

    /// `checkout.prepare`: put a pool checkout on `branch` at `base`.
    async fn prepare(&self, params: Value) -> Result<Value, RemoteError> {
        let request: CheckoutPrepare = parse(params, CHECKOUT_PREPARE)?;
        project_key(&request.project)?;
        self.worker_branch(&request.branch).await?;
        commit_sha(&request.base)?;
        // The pool pick must be serialized per project, or two prepares race
        // for the same free pool.
        let project_lock = self.lock(format!("project:{}", request.project));
        let _project = project_lock.lock().await;
        let repo = self.ensure_repo(&request.project).await?;
        if request.keep_existing {
            if let Some((name, path)) = self.pool_on(&request.project, &request.branch).await {
                let tip = self.head(&path).await?;
                return Ok(json!(CheckoutAnswer {
                    checkout: name,
                    path: path.to_string_lossy().into_owned(),
                    tip,
                    reused: true,
                }));
            }
        }
        let (name, path) = self.pick_pool(&request.project, &repo, &request.base).await?;
        let checkout_lock = self.lock(name.clone());
        let _checkout = checkout_lock.lock().await;
        self.reset_pool(&path, &request.branch, &request.base)
            .await?;
        let tip = self.head(&path).await?;
        Ok(json!(CheckoutAnswer {
            checkout: name,
            path: path.to_string_lossy().into_owned(),
            tip,
            reused: false,
        }))
    }

    /// `checkout.list`: every pool of one project, lowest index first.
    async fn list_checkouts(&self, params: Value) -> Result<Value, RemoteError> {
        let request: CheckoutList = parse(params, CHECKOUT_LIST)?;
        project_key(&request.project)?;
        let mut checkouts = Vec::new();
        for (_, name, path) in self.pools(&request.project) {
            checkouts.push(CheckoutInfo {
                branch: self.branch(&path).await?,
                tip: self.head(&path).await.unwrap_or_default(),
                dirty: !self.dirty(&path).await.unwrap_or_default().is_empty(),
                checkout: name,
                path: path.to_string_lossy().into_owned(),
            });
        }
        Ok(json!(CheckoutListAnswer { checkouts }))
    }

    /// `checkout.release`: detach the pool, drop its branch, trim the extras.
    async fn release(&self, params: Value) -> Result<Value, RemoteError> {
        let request: CheckoutRelease = parse(params, CHECKOUT_RELEASE)?;
        let path = checkout_path(&self.home, &request.checkout)?;
        let project = checkout_project(&request.checkout);
        let checkout_lock = self.lock(request.checkout.clone());
        let _checkout = checkout_lock.lock().await;
        let branch = self.branch(&path).await?;
        self.checkout(&path, &["checkout", "--detach"]).await?;
        self.checkout(&path, &["reset", "--hard"]).await?;
        self.checkout(&path, &["clean", "-fd"]).await?;
        if let Some(branch) = branch.filter(|name| name.starts_with("wt/agent-")) {
            // The branch may already be gone (a pool the hub released twice); a
            // missing branch is not a release failure.
            let _ = self
                .checkout(&path, &["branch", "-D", branch.as_str()])
                .await;
        }
        if let Some(project) = project {
            self.trim_pools(&project).await;
        }
        Ok(json!({}))
    }

    /// `repo.tip`: one checkout's `HEAD`, branch and dirty paths.
    async fn tip(&self, params: Value) -> Result<Value, RemoteError> {
        let request: RepoTip = parse(params, REPO_TIP)?;
        let path = checkout_path(&self.home, &request.checkout)?;
        Ok(json!(TipAnswer {
            tip: self.head(&path).await?,
            branch: self.branch(&path).await?,
            dirty: self.dirty(&path).await?,
        }))
    }

    /// `repo.fetch_branch`: stream the branch's commits the hub lacks.
    async fn fetch_branch(&self, params: Value, out: StreamTx) -> Result<Value, RemoteError> {
        let request: FetchBranch = parse(params, REPO_FETCH_BRANCH)?;
        let bundle = self.branch_bundle(&request).await?;
        if let Err(error) = out.start(json!(bundle.start)).await {
            if let Some(file) = bundle.file {
                let _ = std::fs::remove_file(file);
            }
            return Err(error);
        }
        if let Some(file) = bundle.file {
            let streamed = stream_file(&file, out).await;
            let _ = std::fs::remove_file(&file);
            streamed?;
        }
        Ok(Value::Null)
    }

    /// `checks.run`: run the project's checks in one checkout.
    async fn run_checks(&self, params: Value) -> Result<Value, RemoteError> {
        let request: ChecksRun = parse(params, CHECKS_RUN)?;
        let path = checkout_path(&self.home, &request.checkout)?;
        if request.commands.is_empty() {
            return Err(RemoteError::bad_params(
                "checks.run carries no check commands",
            ));
        }
        if !request.tip.is_empty() {
            commit_sha(&request.tip)?;
        }
        let checkout_lock = self.lock(request.checkout.clone());
        let _checkout = checkout_lock.lock().await;
        let tip_before = self.head(&path).await?;
        let sandbox =
            crate::tool::agent::sandbox::Sandbox::new(path.clone()).map_err(|error| {
                RemoteError::internal(format!(
                    "cannot open the checkout {}: {error}",
                    path.display()
                ))
            })?;
        let mut tool = crate::tool::agent::run_checks::RunChecksTool::new(sandbox)
            .with_check_commands(request.commands);
        // The same slots and the same core-operation list a local session's
        // tool gets. A config that cannot be read runs unslotted with the
        // defaults; the machine limit is never a failure mode.
        let config_dir = self.config_dir();
        if let Ok(config) = crate::config::Config::load(&config_dir) {
            tool = tool
                .with_machine(
                    Arc::new(std::sync::RwLock::new(
                        config.general.general.machine.clone(),
                    )),
                    crate::machine_slots::default_dir(&config_dir),
                )
                .with_core_operations(Arc::new(std::sync::RwLock::new(
                    config.general.git.core_operations.clone(),
                )));
        }
        use crate::tool::Tool as _;
        let result = tool
            .execute(json!({"purpose": "Mnemo running the project checks"}))
            .await;
        let tip_after = self
            .head(&path)
            .await
            .unwrap_or_else(|_| tip_before.clone());
        Ok(json!(ChecksAnswer {
            tip_before,
            tip_after,
            result,
        }))
    }

    // ── internals ───────────────────────────────────────────────────────────

    /// The node's config dir (the parent of its home, or the home itself when
    /// it has no parent).
    fn config_dir(&self) -> PathBuf {
        self.home
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.home.clone())
    }

    /// `<home>/repos/<project>.git`.
    fn repo_dir(&self, project: &str) -> PathBuf {
        self.home.join(REPOS_DIR).join(format!("{project}.git"))
    }

    /// The async lock for one key (created on first use).
    fn lock(&self, key: impl Into<String>) -> Arc<Mutex<()>> {
        let mut locks = self
            .checkout_locks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        locks
            .entry(key.into())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// The project's bare repo, created on first use.
    async fn ensure_repo(&self, project: &str) -> Result<PathBuf, RemoteError> {
        let repo = self.repo_dir(project);
        if repo.is_dir() {
            return Ok(repo);
        }
        if let Some(parent) = repo.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                RemoteError::internal(format!("create {}: {error}", parent.display()))
            })?;
        }
        let text = repo.to_string_lossy().into_owned();
        self.home_git(&["init", "--bare", &text]).await?;
        self.bare(&repo, &["config", "core.autocrlf", "false"])
            .await?;
        self.bare(&repo, &["config", "gc.auto", "0"]).await?;
        // A worker checkout can sit deeper than Windows' 260-character default;
        // git ignores the setting where it has no meaning.
        self.bare(&repo, &["config", "core.longpaths", "true"])
            .await?;
        Ok(repo)
    }

    /// Every pool of `project` as `(index, name, path)`, lowest index first.
    fn pools(&self, project: &str) -> Vec<(usize, String, PathBuf)> {
        let mut pools = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.home.join(CHECKOUTS_DIR)) else {
            return pools;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(index) = pool_index(&name, project) else {
                continue;
            };
            if entry.path().is_dir() {
                pools.push((index, name, entry.path()));
            }
        }
        pools.sort_by_key(|(index, _, _)| *index);
        pools
    }

    /// The pool already checked out on `branch`, if any.
    async fn pool_on(&self, project: &str, branch: &str) -> Option<(String, PathBuf)> {
        for (_, name, path) in self.pools(project) {
            if self.branch(&path).await.ok().flatten().as_deref() == Some(branch) {
                return Some((name, path));
            }
        }
        None
    }

    /// The lowest free pool, or a new one at the next index.
    ///
    /// A pool is free when `HEAD` is not on a `wt/agent-*` branch — a released
    /// pool (detached) or one that never got a branch.
    async fn pick_pool(
        &self,
        project: &str,
        repo: &Path,
        base: &str,
    ) -> Result<(String, PathBuf), RemoteError> {
        let pools = self.pools(project);
        for (_, name, path) in pools.iter() {
            let busy = self
                .branch(path)
                .await
                .ok()
                .flatten()
                .is_some_and(|branch| branch.starts_with("wt/agent-"));
            if !busy {
                return Ok((name.clone(), path.clone()));
            }
        }
        let next = pools.last().map(|(index, _, _)| index + 1).unwrap_or(1);
        let name = format!("{project}-{next}");
        let path = self.home.join(CHECKOUTS_DIR).join(&name);
        let text = path.to_string_lossy().into_owned();
        self.bare(repo, &["worktree", "add", "--detach", &text, base])
            .await?;
        Ok((name, path))
    }

    /// Drop a pool's tree state and put it on `branch` at `base`.
    async fn reset_pool(&self, path: &Path, branch: &str, base: &str) -> Result<(), RemoteError> {
        self.checkout(path, &["reset", "--hard"]).await?;
        // Never `-x`: the ignored `target/` folder is the warm build cache the
        // pool exists for.
        self.checkout(path, &["clean", "-fd"]).await?;
        self.checkout(path, &["checkout", "-B", branch, base])
            .await?;
        Ok(())
    }

    /// Rename and delete every pool above the slot count.
    ///
    /// The rename happens first, so the pool stops being listed the moment the
    /// release answers; the recursive delete and the bare repo's worktree
    /// bookkeeping run on the blocking pool.
    async fn trim_pools(&self, project: &str) {
        let extra: Vec<(String, PathBuf)> = self
            .pools(project)
            .into_iter()
            .filter(|(index, _, _)| *index > self.slots)
            .map(|(_, name, path)| (name, path))
            .collect();
        if extra.is_empty() {
            return;
        }
        let trash = self.home.join(CHECKOUTS_DIR);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        let mut doomed = Vec::new();
        for (name, path) in extra {
            let target = trash.join(format!("trash-{name}-{stamp}"));
            if std::fs::rename(&path, &target).is_ok() {
                doomed.push(target);
            }
        }
        let repo = self.repo_dir(project);
        tokio::task::spawn_blocking(move || {
            for path in doomed {
                let _ = std::fs::remove_dir_all(&path);
            }
            // Drop the deleted worktrees' admin entries; a failure here only
            // leaves stale metadata behind.
            let _ = crate::project::git_ops::git_raw(
                repo.parent().unwrap_or(&repo),
                &[
                    "--git-dir",
                    &repo.to_string_lossy(),
                    "worktree",
                    "prune",
                ],
            );
        });
    }

    /// Remove every upload idle for longer than [`UPLOAD_IDLE`].
    async fn sweep_uploads(&self) {
        let mut uploads = self.uploads.lock().await;
        let idle: Vec<String> = uploads
            .iter()
            .filter(|(_, upload)| upload.last.elapsed() > UPLOAD_IDLE)
            .map(|(id, _)| id.clone())
            .collect();
        for id in idle {
            if let Some(upload) = uploads.remove(&id) {
                let _ = std::fs::remove_file(upload.path);
            }
        }
    }

    /// Verify one upload's bundle and fetch its refs into the bare repo.
    async fn land_bundle(
        &self,
        project: &str,
        bundle: &Path,
        refs: &[BundleRef],
    ) -> Result<Value, RemoteError> {
        let repo = self.ensure_repo(project).await?;
        let text = bundle.to_string_lossy().into_owned();
        if let Err(error) = self.bare(&repo, &["bundle", "verify", &text]).await {
            return Err(RemoteError::bad_params(format!(
                "{REPO_PUSH_END}: the bundle does not verify: {error}"
            )));
        }
        for reference in refs {
            let refspec = format!("+{}:{}", reference.name, reference.name);
            self.bare(&repo, &["fetch", &text, &refspec]).await?;
            let landed = self.bare(&repo, &["rev-parse", &reference.name]).await?;
            if landed.trim() != reference.sha {
                return Err(RemoteError::bad_params(format!(
                    "{REPO_PUSH_END}: {} landed on {}, the bundle promised {}",
                    reference.name,
                    landed.trim(),
                    reference.sha
                )));
            }
        }
        Ok(json!({}))
    }

    /// Prepare the bundle a `repo.fetch_branch` streams (or the `unchanged`
    /// answer when the hub already has the tip).
    async fn branch_bundle(&self, request: &FetchBranch) -> Result<BranchBundle, RemoteError> {
        let path = checkout_path(&self.home, &request.checkout)?;
        self.worker_branch(&request.branch).await?;
        for sha in &request.haves {
            commit_sha(sha)?;
        }
        let checkout_lock = self.lock(request.checkout.clone());
        let _checkout = checkout_lock.lock().await;
        let tip = self.head(&path).await?;
        let dirty = self.dirty(&path).await?;
        let mut haves = Vec::new();
        for sha in &request.haves {
            let probe = format!("{sha}^{{commit}}");
            if self
                .checkout(&path, &["cat-file", "-e", &probe])
                .await
                .is_ok()
            {
                haves.push(sha.clone());
            }
        }
        let unchanged = BranchBundle {
            start: FetchStart {
                tip: tip.clone(),
                dirty: dirty.clone(),
                size: 0,
                sha256: String::new(),
                unchanged: true,
            },
            file: None,
        };
        if haves.iter().any(|sha| sha == &tip) {
            return Ok(unchanged);
        }
        let bundle = self
            .home
            .join(TMP_DIR)
            .join(format!("{}.bundle", uuid::Uuid::new_v4().simple()));
        let mut args = vec![
            "bundle".to_string(),
            "create".to_string(),
            bundle.to_string_lossy().into_owned(),
            format!("refs/heads/{}", request.branch),
        ];
        args.extend(haves.iter().map(|sha| format!("^{sha}")));
        let borrow: Vec<&str> = args.iter().map(String::as_str).collect();
        match self.checkout(&path, &borrow).await {
            Ok(_) => {}
            // Git refuses to write a bundle with nothing in it: the hub already
            // has every commit the branch reaches.
            Err(error) if error.message.contains("empty bundle") => return Ok(unchanged),
            Err(error) => return Err(error),
        }
        let (size, sha256) = digest_file(&bundle).await?;
        Ok(BranchBundle {
            start: FetchStart {
                tip,
                dirty,
                size,
                sha256,
                unchanged: false,
            },
            file: Some(bundle),
        })
    }

    /// `HEAD`'s commit sha of one checkout.
    async fn head(&self, path: &Path) -> Result<String, RemoteError> {
        Ok(self
            .checkout(path, &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string())
    }

    /// The branch `HEAD` is on, `None` while detached.
    async fn branch(&self, path: &Path) -> Result<Option<String>, RemoteError> {
        let name = self
            .checkout(path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await?
            .trim()
            .to_string();
        Ok(if name.is_empty() || name == "HEAD" {
            None
        } else {
            Some(name)
        })
    }

    /// Up to 50 `git status --porcelain` lines.
    async fn dirty(&self, path: &Path) -> Result<Vec<String>, RemoteError> {
        let status = self.checkout(path, &["status", "--porcelain"]).await?;
        Ok(status
            .lines()
            .take(50)
            .map(str::to_string)
            .collect::<Vec<_>>())
    }

    /// Whether `name` is a full ref git accepts (`git check-ref-format`).
    async fn check_ref(&self, name: &str) -> Result<(), RemoteError> {
        let name = name.to_string();
        self.home_git(&["check-ref-format", name.as_str()])
            .await
            .map_err(|_| RemoteError::bad_params(format!("'{name}' is not a valid ref name")))?;
        Ok(())
    }

    /// Refuse anything that is not a worker branch git accepts.
    ///
    /// The `wt/agent-*` prefix keeps the hub from driving a branch the lead
    /// branch owns; `git check-ref-format --branch` rejects the ref spellings
    /// git itself would (a trailing dot, `..`, `@{`, …).
    async fn worker_branch(&self, branch: &str) -> Result<(), RemoteError> {
        if !branch.starts_with("wt/agent-") {
            return Err(RemoteError::bad_params(format!(
                "'{branch}' is not a worker branch (wt/agent-*)"
            )));
        }
        self.home_git(&["check-ref-format", "--branch", branch])
            .await
            .map_err(|_| {
                RemoteError::bad_params(format!("'{branch}' is not a valid branch name"))
            })?;
        Ok(())
    }

    /// Run one git command with `path` as the working tree.
    async fn checkout(&self, path: &Path, args: &[&str]) -> Result<String, RemoteError> {
        self.git(path.to_path_buf(), args).await
    }

    /// Run one git command against a bare repo (`--git-dir`), from the home.
    async fn bare(&self, repo: &Path, args: &[&str]) -> Result<String, RemoteError> {
        let owned: Vec<String> = std::iter::once("--git-dir".to_string())
            .chain(std::iter::once(repo.to_string_lossy().into_owned()))
            .chain(args.iter().map(|arg| (*arg).to_string()))
            .collect();
        self.git(
            self.home.clone(),
            &owned.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .await
    }

    /// Run one git command from the node home.
    async fn home_git(&self, args: &[&str]) -> Result<String, RemoteError> {
        self.git(self.home.clone(), args).await
    }

    /// Run one git command on the blocking pool.
    async fn git(&self, root: PathBuf, args: &[&str]) -> Result<String, RemoteError> {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let borrow: Vec<&str> = owned.iter().map(String::as_str).collect();
            crate::project::git_ops::git_raw(&root, &borrow)
        })
        .await
        .map_err(|error| RemoteError::internal(format!("git task failed: {error}")))?
        .map_err(RemoteError::internal)
    }
}

/// Stream `file` to `out` in [`BUNDLE_CHUNK`] pieces, base64 each.
async fn stream_file(file: &Path, out: StreamTx) -> Result<(), RemoteError> {
    use tokio::io::AsyncReadExt as _;
    let mut handle = tokio::fs::File::open(file)
        .await
        .map_err(|error| RemoteError::internal(format!("open {}: {error}", file.display())))?;
    let mut buffer = vec![0u8; BUNDLE_CHUNK];
    loop {
        let read = handle
            .read(&mut buffer)
            .await
            .map_err(|error| RemoteError::internal(format!("read {}: {error}", file.display())))?;
        if read == 0 {
            return Ok(());
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&buffer[..read]);
        out.item(json!({ "data": data })).await?;
    }
}

/// One `req` payload, or a `bad_params` naming the method.
fn parse<T: serde::de::DeserializeOwned>(params: Value, method: &str) -> Result<T, RemoteError> {
    serde_json::from_value(params)
        .map_err(|error| RemoteError::bad_params(format!("bad {method}: {error}")))
}

/// A project key: exactly 12 lowercase hex digits.
fn project_key(project: &str) -> Result<(), RemoteError> {
    let valid = project.len() == 12
        && project
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(RemoteError::bad_params(format!(
            "'{project}' is not a project key (12 hex digits)"
        )))
    }
}

/// A commit sha: 40 (sha1) or 64 (sha256) hex digits.
fn commit_sha(sha: &str) -> Result<(), RemoteError> {
    let valid =
        (sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(RemoteError::bad_params(format!(
            "'{sha}' is not a commit sha"
        )))
    }
}

/// A sha256 digest: exactly 64 hex digits.
fn hex_digest(value: &str) -> Result<(), RemoteError> {
    let valid = value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(RemoteError::bad_params(format!(
            "'{value}' is not a sha256 digest"
        )))
    }
}

/// The index of a `<project>-<n>` pool name.
fn pool_index(name: &str, project: &str) -> Option<usize> {
    let rest = name.strip_prefix(project)?.strip_prefix('-')?;
    rest.parse::<usize>().ok().filter(|index| *index > 0)
}

/// The project a `<project>-<n>` checkout name belongs to.
fn checkout_project(checkout: &str) -> Option<String> {
    let (project, index) = checkout.rsplit_once('-')?;
    index.parse::<usize>().ok()?;
    project_key(project).ok()?;
    Some(project.to_string())
}

/// Write `data` at `offset` in `path` (which `repo.push_begin` created).
async fn write_at(path: &Path, offset: u64, data: &[u8]) -> Result<(), RemoteError> {
    use std::io::{Seek as _, SeekFrom, Write as _};
    let path = path.to_path_buf();
    let data = data.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| RemoteError::internal(format!("open {}: {error}", path.display())))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| RemoteError::internal(format!("seek {}: {error}", path.display())))?;
        file.write_all(&data)
            .map_err(|error| RemoteError::internal(format!("write {}: {error}", path.display())))
    })
    .await
    .map_err(|error| RemoteError::internal(format!("write task failed: {error}")))?
}

/// Check one upload's size and digest before it is fetched.
async fn verify_file(path: &Path, size: u64, sha256: &str) -> Result<(), RemoteError> {
    let (actual_size, actual_digest) = digest_file(path).await?;
    if actual_size != size {
        return Err(RemoteError::bad_params(format!(
            "the upload is {actual_size} bytes, repo.push_begin promised {size}"
        )));
    }
    if !actual_digest.eq_ignore_ascii_case(sha256) {
        return Err(RemoteError::bad_params(format!(
            "the upload's sha256 is {actual_digest}, repo.push_begin promised {sha256}"
        )));
    }
    Ok(())
}

/// One file's size and sha256 (hex).
async fn digest_file(path: &Path) -> Result<(u64, String), RemoteError> {
    use std::io::Read as _;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)
            .map_err(|error| RemoteError::internal(format!("open {}: {error}", path.display())))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut size = 0u64;
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                RemoteError::internal(format!("read {}: {error}", path.display()))
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size += read as u64;
        }
        Ok((size, crate::remote::auth::hex_encode(&hasher.finalize())))
    })
    .await
    .map_err(|error| RemoteError::internal(format!("digest task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// A scratch directory under `target/` (never `%TEMP%`).
    fn scratch(tag: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&root).expect("target dir");
        let dir = root.join(format!("mnemo-remote-repo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Run one git command in `dir`, panicking on failure.
    fn git(dir: &Path, args: &[&str]) -> String {
        crate::project::git_ops::git_raw(dir, args).expect("git command")
    }

    /// A source repo with one commit on `main`, answering that commit's sha.
    ///
    /// `.gitignore` names `target/` exactly as the real project does, so
    /// `clean -fd` has the same view of the warm build cache a node pool keeps.
    fn source_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).expect("source dir");
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.name", "Mnemo Test"]);
        git(dir, &["config", "user.email", "test@mnemo.invalid"]);
        std::fs::write(dir.join(".gitignore"), "target/\n").expect("write");
        std::fs::write(dir.join("README.md"), "hello\n").expect("write");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "one"]);
        git(dir, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// Give the node's bare repo a commit identity, so a test commit made in a
    /// pool checkout never depends on the machine's global git config.
    fn set_identity(host: &RepoHost, project: &str) {
        let repo = host.repo_dir(project);
        let text = repo.to_string_lossy().into_owned();
        git(
            &host.home,
            &["--git-dir", &text, "config", "user.name", "Mnemo Test"],
        );
        git(
            &host.home,
            &[
                "--git-dir",
                &text,
                "config",
                "user.email",
                "test@mnemo.invalid",
            ],
        );
    }

    /// The `repo.push_*` round trip that lands `source`'s `branch` in the
    /// node's bare repo, answering that branch's tip.
    async fn push_branch(host: &RepoHost, project: &str, source: &Path, branch: &str) -> String {
        let bundle = source.with_extension("bundle");
        let text = bundle.to_string_lossy().into_owned();
        git(source, &["bundle", "create", &text, branch]);
        let bytes = std::fs::read(&bundle).expect("read bundle");
        let digest = crate::remote::auth::hex_encode(&Sha256::digest(&bytes));
        let started = host
            .push_begin(json!({
                "project": project,
                "size": bytes.len(),
                "sha256": digest,
            }))
            .await
            .expect("push_begin");
        let upload = started["upload"].as_str().expect("upload id").to_string();
        host.push_chunk(json!({
            "upload": upload,
            "offset": 0,
            "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }))
        .await
        .expect("push_chunk");
        let sha = git(source, &["rev-parse", branch]).trim().to_string();
        host.push_end(json!({
            "upload": upload,
            "refs": [{ "name": format!("refs/heads/{branch}"), "sha": sha }],
        }))
        .await
        .expect("push_end");
        sha
    }

    /// The `repo.fetch_branch` answer for one checkout, without a stream.
    async fn fetch_request(
        host: &RepoHost,
        checkout: &str,
        branch: &str,
        haves: &[String],
    ) -> BranchBundle {
        host.branch_bundle(&FetchBranch {
            checkout: checkout.to_string(),
            branch: branch.to_string(),
            haves: haves.to_vec(),
        })
        .await
        .expect("fetch_branch")
    }

    #[tokio::test]
    async fn prepare_reuses_a_free_pool_and_keeps_target() {
        let root = scratch("prepare-reuse");
        let source = root.join("source");
        let base = source_repo(&source);
        let host = RepoHost::new(root.join("home"), 2);
        let project = "0123456789ab";
        push_branch(&host, project, &source, "main").await;

        let first = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-one",
                "base": base,
            }))
            .await
            .expect("first prepare");
        let first: CheckoutAnswer = serde_json::from_value(first).expect("answer");
        let path = PathBuf::from(&first.path);
        assert!(path.is_dir(), "the checkout should exist");
        assert_eq!(first.tip, base);
        assert!(!first.reused);

        // A warm build cache the pool must keep across a release.
        std::fs::create_dir_all(path.join("target")).expect("target");
        std::fs::write(path.join("target").join("warm"), "cache").expect("write");

        host.release(json!({ "checkout": first.checkout }))
            .await
            .expect("release");

        let second = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-two",
                "base": base,
            }))
            .await
            .expect("second prepare");
        let second: CheckoutAnswer = serde_json::from_value(second).expect("answer");
        assert_eq!(
            second.checkout, first.checkout,
            "the free pool should be reused, not replaced"
        );
        assert_eq!(second.tip, base);
        assert!(
            Path::new(&second.path).join("target").join("warm").is_file(),
            "clean -fd must not touch the ignored target folder"
        );
    }

    #[tokio::test]
    async fn prepare_keep_existing_returns_the_same_checkout() {
        let root = scratch("prepare-keep");
        let source = root.join("source");
        let base = source_repo(&source);
        let host = RepoHost::new(root.join("home"), 2);
        let project = "0123456789ab";
        push_branch(&host, project, &source, "main").await;

        let first = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-keep",
                "base": base,
            }))
            .await
            .expect("prepare");
        let first: CheckoutAnswer = serde_json::from_value(first).expect("answer");
        set_identity(&host, project);

        // A commit the worker made: `keep_existing` must not reset it away.
        std::fs::write(Path::new(&first.path).join("work.txt"), "in progress\n").expect("write");
        git(Path::new(&first.path), &["add", "-A"]);
        git(Path::new(&first.path), &["commit", "-q", "-m", "work"]);
        let ahead = git(Path::new(&first.path), &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        let again = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-keep",
                "base": base,
                "keep_existing": true,
            }))
            .await
            .expect("prepare again");
        let again: CheckoutAnswer = serde_json::from_value(again).expect("answer");
        assert_eq!(again.checkout, first.checkout);
        assert!(again.reused, "keep_existing should report the reuse");
        assert_eq!(again.tip, ahead, "the worker's commit must survive");
    }

    #[tokio::test]
    async fn fetch_branch_bundle_round_trips() {
        let root = scratch("fetch-round-trip");
        let source = root.join("source");
        let base = source_repo(&source);
        let host = RepoHost::new(root.join("home"), 2);
        let project = "0123456789ab";
        push_branch(&host, project, &source, "main").await;

        let prepared = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-round",
                "base": base,
            }))
            .await
            .expect("prepare");
        let prepared: CheckoutAnswer = serde_json::from_value(prepared).expect("answer");
        let checkout = PathBuf::from(&prepared.path);
        set_identity(&host, project);
        std::fs::write(checkout.join("work.txt"), "done\n").expect("write");
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-q", "-m", "work"]);
        let tip = git(&checkout, &["rev-parse", "HEAD"]).trim().to_string();

        // The bundle the node hands back, fetched into a second repo: exactly
        // the round trip the hub performs.
        let bundle = fetch_request(
            &host,
            &prepared.checkout,
            "wt/agent-round",
            std::slice::from_ref(&base),
        )
        .await;
        assert!(!bundle.start.unchanged, "there is a new commit to send");
        assert_eq!(bundle.start.tip, tip);
        let file = bundle.file.expect("a bundle file");
        let (size, digest) = digest_file(&file).await.expect("digest");
        assert_eq!(size, bundle.start.size);
        assert_eq!(digest, bundle.start.sha256);

        // The hub already holds the base commit, which is why the bundle's
        // history is cut there; seed the fetch target the same way.
        let second = root.join("second");
        std::fs::create_dir_all(&second).expect("second dir");
        git(&second, &["init", "-q"]);
        let source_text = source.to_string_lossy().into_owned();
        git(
            &second,
            &[
                "fetch",
                &source_text,
                "+refs/heads/main:refs/heads/main",
            ],
        );
        let text = file.to_string_lossy().into_owned();
        git(
            &second,
            &[
                "fetch",
                &text,
                "+refs/heads/wt/agent-round:refs/heads/wt/agent-round",
            ],
        );
        let landed = git(&second, &["rev-parse", "refs/heads/wt/agent-round"])
            .trim()
            .to_string();
        assert_eq!(landed, tip, "the bundle should carry the worker's commit");
    }

    #[tokio::test]
    async fn fetch_branch_unchanged_when_tip_is_a_have() {
        let root = scratch("fetch-unchanged");
        let source = root.join("source");
        let base = source_repo(&source);
        let host = RepoHost::new(root.join("home"), 2);
        let project = "0123456789ab";
        push_branch(&host, project, &source, "main").await;

        let prepared = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-same",
                "base": base,
            }))
            .await
            .expect("prepare");
        let prepared: CheckoutAnswer = serde_json::from_value(prepared).expect("answer");

        let bundle = fetch_request(
            &host,
            &prepared.checkout,
            "wt/agent-same",
            std::slice::from_ref(&base),
        )
        .await;
        assert!(bundle.start.unchanged, "the hub already has the tip");
        assert_eq!(bundle.start.tip, base);
        assert!(bundle.start.size == 0 && bundle.start.sha256.is_empty());
        assert!(bundle.file.is_none());
    }

    #[tokio::test]
    async fn push_end_rejects_a_bad_sha256() {
        let root = scratch("push-bad-sha");
        let source = root.join("source");
        source_repo(&source);
        let host = RepoHost::new(root.join("home"), 2);
        let bundle = source.with_extension("bundle");
        let text = bundle.to_string_lossy().into_owned();
        git(&source, &["bundle", "create", &text, "refs/heads/main"]);
        let bytes = std::fs::read(&bundle).expect("read bundle");
        let started = host
            .push_begin(json!({
                "project": "0123456789ab",
                "size": bytes.len(),
                "sha256": "0".repeat(64),
            }))
            .await
            .expect("push_begin");
        let upload = started["upload"].as_str().expect("upload").to_string();
        host.push_chunk(json!({
            "upload": upload,
            "offset": 0,
            "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }))
        .await
        .expect("push_chunk");
        let error = host
            .push_end(json!({ "upload": upload, "refs": [] }))
            .await
            .expect_err("a wrong digest must be refused");
        assert_eq!(error.code, crate::remote::CODE_BAD_PARAMS);
        assert!(error.message.contains("sha256"), "{}", error.message);
    }

    #[tokio::test]
    async fn release_detaches_and_trims_above_slots() {
        let root = scratch("release-trim");
        let source = root.join("source");
        let base = source_repo(&source);
        let host = RepoHost::new(root.join("home"), 1);
        let project = "0123456789ab";
        push_branch(&host, project, &source, "main").await;

        // Two pools, the second one only because the first is busy.
        let first = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-a",
                "base": base,
            }))
            .await
            .expect("first");
        let first: CheckoutAnswer = serde_json::from_value(first).expect("answer");
        let second = host
            .prepare(json!({
                "project": project,
                "branch": "wt/agent-b",
                "base": base,
            }))
            .await
            .expect("second");
        let second: CheckoutAnswer = serde_json::from_value(second).expect("answer");
        assert_ne!(first.checkout, second.checkout);

        host.release(json!({ "checkout": second.checkout }))
            .await
            .expect("release");

        let answer = host
            .list_checkouts(json!({ "project": project }))
            .await
            .expect("list");
        let answer: CheckoutListAnswer = serde_json::from_value(answer).expect("answer");
        let names: Vec<&str> = answer
            .checkouts
            .iter()
            .map(|checkout| checkout.checkout.as_str())
            .collect();
        assert_eq!(
            names,
            vec![first.checkout.as_str()],
            "the pool above the slot count should be trimmed"
        );
        assert_eq!(
            answer.checkouts[0].branch.as_deref(),
            Some("wt/agent-a"),
            "the surviving pool is still on its worker branch"
        );
        assert!(
            !Path::new(&second.path).exists(),
            "the trimmed pool's folder is gone"
        );
    }

    #[tokio::test]
    async fn bad_project_or_branch_is_bad_params() {
        let root = scratch("bad-params");
        let host = RepoHost::new(root.join("home"), 2);

        let error = host
            .have(json!({ "project": "nope", "want": [] }))
            .await
            .expect_err("a bad project key is refused");
        assert_eq!(error.code, crate::remote::CODE_BAD_PARAMS);

        let error = host
            .prepare(json!({
                "project": "0123456789ab",
                "branch": "main",
                "base": "a".repeat(40),
            }))
            .await
            .expect_err("a non-worker branch is refused");
        assert_eq!(error.code, crate::remote::CODE_BAD_PARAMS);

        let error = host
            .prepare(json!({
                "project": "0123456789ab",
                "branch": "wt/agent-bad..name",
                "base": "a".repeat(40),
            }))
            .await
            .expect_err("an invalid ref name is refused");
        assert_eq!(error.code, crate::remote::CODE_BAD_PARAMS);

        let error = host
            .tip(json!({ "checkout": "../escape" }))
            .await
            .expect_err("a path escape is refused");
        assert_eq!(error.code, crate::remote::CODE_BAD_PARAMS);
    }
}
