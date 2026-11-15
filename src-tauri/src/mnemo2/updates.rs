// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo2 updates itself: a background check against the GitHub release feed,
//! a quiet download, and an install at restart.
//!
//! The host owns the cycle and reports every change on
//! [`UPDATE_STATE_CHANNEL`], which the About page and the update banner
//! mirror: `update_state` answers the current state, `update_check` starts a
//! check at once, and `update_restart` arms the install for the next orderly
//! shutdown (`install_if_requested`), which then installs and relaunches. A
//! found update is downloaded quietly; nothing is installed until the user
//! asks for the restart, and a plain quit never installs.
//!
//! A build without the `auto-update` feature — a source build such as
//! `npm run rebuild:mnemo2` or a plain `cargo build` — starts `disabled` with
//! reason `source_build` and never contacts GitHub.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

/// The channel every state change is reported on (the frontend's
/// `UPDATE_EVENT_CHANNEL` in `mnemo2/src/lib/updates.ts`).
pub const UPDATE_STATE_CHANNEL: &str = "mnemo2://update-state";

/// How long after start the first background check runs.
pub(crate) const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);

/// How often the background loop checks again while no update is ready.
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// While a download reports no total size, emit progress at most once per this
/// many downloaded bytes.
const UNKNOWN_TOTAL_STEP: u64 = 1024 * 1024;

/// What `request_install` and `update_restart` answer when nothing is downloaded.
const NOT_READY: &str = "No update is ready to install.";

/// What the host reports about its update cycle.
///
/// Serialized with a snake_case `phase` tag — the wire shape of the frontend's
/// `UpdateState` union (`mnemo2/src/lib/updates.ts`).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum UpdateState {
    /// This build never updates itself.
    Disabled {
        /// Why (`source_build`).
        reason: String,
    },
    /// No update is pending.
    Idle {
        /// When the last completed check ran, in UNIX milliseconds.
        last_checked_ms: Option<u64>,
    },
    /// A check is running.
    Checking,
    /// A newer release is downloading.
    Downloading {
        /// The version being downloaded.
        version: String,
        /// How many bytes have arrived so far.
        downloaded: u64,
        /// The release's total size, when the server reports it.
        total: Option<u64>,
    },
    /// The release is downloaded and waits for the user's restart.
    Ready {
        /// The downloaded version.
        version: String,
        /// The release notes (`latest.json`'s `notes`), when present.
        notes: Option<String>,
    },
    /// The last check or download failed; the next interval retries.
    Failed {
        /// What went wrong.
        message: String,
        /// When the failed check ran, in UNIX milliseconds.
        last_checked_ms: Option<u64>,
    },
}

/// The answer to `update_restart`.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum UpdateRestart {
    /// The orderly shutdown — and the install behind it — started.
    Started,
    /// Workers are running; the user is asked before they are interrupted.
    WorkersRunning {
        /// How many workers are running.
        count: usize,
    },
}

/// The state a fresh process starts in: `idle` with the updater compiled in,
/// otherwise `disabled` because this is a source build.
pub(crate) fn initial_state() -> UpdateState {
    #[cfg(feature = "auto-update")]
    {
        UpdateState::Idle {
            last_checked_ms: None,
        }
    }
    #[cfg(not(feature = "auto-update"))]
    {
        UpdateState::Disabled {
            reason: "source_build".to_string(),
        }
    }
}

/// Whether a check may start from this state: only when idle or after a
/// failure (a failed check retries on the next interval).
pub(crate) fn should_check(state: &UpdateState) -> bool {
    matches!(state, UpdateState::Idle { .. } | UpdateState::Failed { .. })
}

/// Where update checks go: the public GitHub release feed in the real host, a
/// script in the tests.
#[async_trait::async_trait]
pub(crate) trait UpdateSource: Send + Sync {
    /// Look for a newer release; `None` when this build is up to date.
    async fn check(&self) -> Result<Option<Box<dyn FoundUpdate>>, String>;
}

/// A release the source found, not yet downloaded.
#[async_trait::async_trait]
pub(crate) trait FoundUpdate: Send {
    /// The release's version.
    fn version(&self) -> String;

    /// The release notes, when the feed carries them.
    fn notes(&self) -> Option<String>;

    /// Download the release, calling `progress(downloaded_so_far, total)`
    /// with the cumulative byte count as chunks arrive.
    async fn download(
        self: Box<Self>,
        progress: Box<dyn FnMut(u64, Option<u64>) + Send>,
    ) -> Result<Box<dyn PendingInstall>, String>;
}

/// A downloaded release, installable only through the restart path.
pub(crate) trait PendingInstall: Send {
    /// Install the release. On Windows the installer starts and the process
    /// exits inside this call; elsewhere the caller restarts the app next.
    fn install(self: Box<Self>) -> Result<(), String>;
}

/// The updater service: the current state, the downloaded-but-not-installed
/// release, and the restart request that arms the install at exit.
pub struct UpdateService {
    state: Mutex<UpdateState>,
    pending: Mutex<Option<Box<dyn PendingInstall>>>,
    install_requested: AtomicBool,
    emit: Box<dyn Fn(&UpdateState) + Send + Sync>,
}

impl UpdateService {
    /// A service in `initial` that reports every state change through `emit`.
    pub(crate) fn new(
        initial: UpdateState,
        emit: impl Fn(&UpdateState) + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Mutex::new(initial),
            pending: Mutex::new(None),
            install_requested: AtomicBool::new(false),
            emit: Box::new(emit),
        }
    }

    /// The current state.
    pub(crate) fn state(&self) -> UpdateState {
        self.state.lock().expect("update state lock").clone()
    }

    /// Store `next` and report it, with the lock already released.
    fn set(&self, next: UpdateState) {
        {
            let mut state = self.state.lock().expect("update state lock");
            *state = next.clone();
        }
        (self.emit)(&next);
    }

    /// Move to `checking` when the current state allows a check. `false` while
    /// a check or download runs, while an update is ready, and in a build that
    /// never updates.
    pub(crate) fn begin_check(&self) -> bool {
        let started = {
            let mut state = self.state.lock().expect("update state lock");
            let start = should_check(&state);
            if start {
                *state = UpdateState::Checking;
            }
            start
        };
        if started {
            (self.emit)(&UpdateState::Checking);
        }
        started
    }

    /// Run one check and, when a release is found, download it. `begin_check`
    /// must have returned true; every state change is reported through the
    /// service's emitter.
    pub(crate) async fn run_check(self: &Arc<Self>, source: &dyn UpdateSource, now_ms: u64) {
        let found = match source.check().await {
            Ok(Some(found)) => found,
            Ok(None) => {
                self.set(UpdateState::Idle {
                    last_checked_ms: Some(now_ms),
                });
                return;
            }
            Err(message) => {
                self.set(UpdateState::Failed {
                    message,
                    last_checked_ms: Some(now_ms),
                });
                return;
            }
        };

        let version = found.version();
        let notes = found.notes();
        self.set(UpdateState::Downloading {
            version: version.clone(),
            downloaded: 0,
            total: None,
        });

        let service = Arc::clone(self);
        let progress_version = version.clone();
        // The emitted `downloading 0` above is the 0 % mark; only a different
        // whole percent (or, with no total, a further megabyte) is reported.
        let mut last_percent: Option<u64> = Some(0);
        let mut last_unknown_emit = 0u64;
        let progress: Box<dyn FnMut(u64, Option<u64>) + Send> = Box::new(move |downloaded, total| {
            let next = match total {
                Some(total) if total > 0 => {
                    let percent = downloaded.saturating_mul(100) / total;
                    if last_percent == Some(percent) {
                        None
                    } else {
                        last_percent = Some(percent);
                        Some(UpdateState::Downloading {
                            version: progress_version.clone(),
                            downloaded,
                            total: Some(total),
                        })
                    }
                }
                _ => {
                    if downloaded.saturating_sub(last_unknown_emit) < UNKNOWN_TOTAL_STEP {
                        None
                    } else {
                        last_unknown_emit = downloaded;
                        Some(UpdateState::Downloading {
                            version: progress_version.clone(),
                            downloaded,
                            total: None,
                        })
                    }
                }
            };
            if let Some(next) = next {
                service.set(next);
            }
        });

        match found.download(progress).await {
            Ok(pending) => {
                *self.pending.lock().expect("update pending lock") = Some(pending);
                self.set(UpdateState::Ready { version, notes });
            }
            Err(error) => {
                self.set(UpdateState::Failed {
                    message: format!("download of {version} failed: {error}"),
                    last_checked_ms: Some(now_ms),
                });
            }
        }
    }

    /// Arm the install for a downloaded release. `Err` when no update is ready
    /// (or when the last one was already taken).
    pub(crate) fn request_install(&self) -> Result<(), String> {
        let ready = matches!(
            &*self.state.lock().expect("update state lock"),
            UpdateState::Ready { .. }
        );
        let pending = self
            .pending
            .lock()
            .expect("update pending lock")
            .is_some();
        if ready && pending {
            self.install_requested.store(true, Ordering::SeqCst);
            Ok(())
        } else {
            Err(NOT_READY.to_string())
        }
    }

    /// The armed install, once: the exit path takes it, so a second call finds
    /// nothing.
    pub(crate) fn take_requested_install(&self) -> Option<Box<dyn PendingInstall>> {
        if !self.install_requested.swap(false, Ordering::SeqCst) {
            return None;
        }
        self.pending.lock().expect("update pending lock").take()
    }
}

/// The source a build without the updater has: there is no release feed to
/// ask, and `update_check` never gets past the `disabled` state to use it.
#[cfg(not(feature = "auto-update"))]
struct SourceBuild;

#[cfg(not(feature = "auto-update"))]
#[async_trait::async_trait]
impl UpdateSource for SourceBuild {
    async fn check(&self) -> Result<Option<Box<dyn FoundUpdate>>, String> {
        Ok(None)
    }
}

#[cfg(feature = "auto-update")]
mod plugin {
    //! The real update source: `tauri-plugin-updater` over the public GitHub
    //! release feed (`plugins.updater` in `mnemo2-app/tauri.conf.json`).

    use super::{FoundUpdate, PendingInstall, UpdateSource};

    /// The installed app's release feed.
    pub(super) struct PluginSource {
        pub(super) app: tauri::AppHandle,
    }

    #[async_trait::async_trait]
    impl UpdateSource for PluginSource {
        async fn check(&self) -> Result<Option<Box<dyn FoundUpdate>>, String> {
            use tauri_plugin_updater::UpdaterExt as _;

            let updater = self.app.updater().map_err(|error| error.to_string())?;
            let found = updater.check().await.map_err(|error| error.to_string())?;
            Ok(found.map(|update| Box::new(PluginUpdate { update }) as Box<dyn FoundUpdate>))
        }
    }

    /// A release the feed reports.
    struct PluginUpdate {
        update: tauri_plugin_updater::Update,
    }

    #[async_trait::async_trait]
    impl FoundUpdate for PluginUpdate {
        fn version(&self) -> String {
            self.update.version.clone()
        }

        fn notes(&self) -> Option<String> {
            self.update.body.clone()
        }

        async fn download(
            self: Box<Self>,
            mut progress: Box<dyn FnMut(u64, Option<u64>) + Send>,
        ) -> Result<Box<dyn PendingInstall>, String> {
            let mut downloaded = 0u64;
            let bytes = self
                .update
                .download(
                    move |chunk, total| {
                        downloaded += chunk as u64;
                        progress(downloaded, total);
                    },
                    || {},
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Box::new(PluginPending {
                update: self.update,
                bytes,
            }))
        }
    }

    /// A downloaded release, waiting for the restart that installs it.
    struct PluginPending {
        update: tauri_plugin_updater::Update,
        bytes: Vec<u8>,
    }

    impl PendingInstall for PluginPending {
        fn install(self: Box<Self>) -> Result<(), String> {
            self.update
                .install(self.bytes)
                .map_err(|error| error.to_string())
        }
    }
}

/// The wall clock as milliseconds since the UNIX epoch (0 when the system
/// clock reads before it).
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The release feed this build checks: the plugin's GitHub feed with the
/// updater compiled in, otherwise the no-op source of a source build.
fn update_source(app: &tauri::AppHandle) -> Box<dyn UpdateSource> {
    #[cfg(feature = "auto-update")]
    {
        Box::new(plugin::PluginSource { app: app.clone() })
    }
    #[cfg(not(feature = "auto-update"))]
    {
        let _ = app;
        Box::new(SourceBuild)
    }
}

/// Start the background loop: check [`FIRST_CHECK_DELAY`] after start, then
/// every [`CHECK_INTERVAL`] while no update is ready. A source build starts
/// `disabled`, so its loop ends before the first check and nothing is ever
/// fetched.
pub(crate) fn start(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tauri::Manager as _;

        let service = app.state::<Arc<UpdateService>>().inner().clone();
        if matches!(service.state(), UpdateState::Disabled { .. }) {
            return;
        }
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let service = app.state::<Arc<UpdateService>>().inner().clone();
            if service.begin_check() {
                service.run_check(update_source(&app).as_ref(), now_ms()).await;
            }
            if matches!(service.state(), UpdateState::Ready { .. }) {
                break;
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// The updater's current state (Settings → About, the update banner).
#[tauri::command]
pub async fn update_state(
    service: tauri::State<'_, Arc<UpdateService>>,
) -> Result<UpdateState, String> {
    Ok(service.state())
}

/// Check for an update now (About → Check now); answers at once, progress
/// follows on [`UPDATE_STATE_CHANNEL`].
#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> Result<UpdateState, String> {
    use tauri::Manager as _;

    let service = app.state::<Arc<UpdateService>>().inner().clone();
    if service.begin_check() {
        let source = update_source(&app);
        let running = Arc::clone(&service);
        tauri::async_runtime::spawn(async move {
            running.run_check(source.as_ref(), now_ms()).await;
        });
    }
    Ok(service.state())
}

/// Install the downloaded update and restart (the banner's *Restart now*).
#[tauri::command]
pub async fn update_restart(
    app: tauri::AppHandle,
    force: bool,
) -> Result<UpdateRestart, String> {
    use tauri::Manager as _;

    let service = app.state::<Arc<UpdateService>>().inner().clone();
    if !matches!(service.state(), UpdateState::Ready { .. }) {
        return Err(NOT_READY.to_string());
    }
    if !force {
        let count = super::running_workers(&app).await;
        if count > 0 {
            return Ok(UpdateRestart::WorkersRunning { count });
        }
    }
    service.request_install()?;
    app.exit(0);
    Ok(UpdateRestart::Started)
}

/// Install the update the user asked to restart into, if any.
///
/// `false` when nothing was armed or the install failed; the caller then
/// performs the normal exit. On success the app restarts and this never
/// returns.
pub(crate) fn install_if_requested(app: &tauri::AppHandle) -> bool {
    use tauri::Manager as _;

    let Some(service) = app.try_state::<Arc<UpdateService>>() else {
        return false;
    };
    let Some(pending) = service.take_requested_install() else {
        return false;
    };
    match pending.install() {
        Ok(()) => app.restart(),
        Err(error) => {
            eprintln!("mnemo2: could not install the update: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// An emitter that records every state the service reports.
    fn recording() -> (
        Arc<Mutex<Vec<UpdateState>>>,
        impl Fn(&UpdateState) + Send + Sync + 'static,
    ) {
        let states = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&states);
        let emit = move |state: &UpdateState| {
            sink.lock().expect("recorded states").push(state.clone());
        };
        (states, emit)
    }

    /// A service in `initial` plus its recorder.
    fn fresh(
        initial: UpdateState,
    ) -> (Arc<UpdateService>, Arc<Mutex<Vec<UpdateState>>>) {
        let (states, emit) = recording();
        (Arc::new(UpdateService::new(initial, emit)), states)
    }

    fn idle() -> UpdateState {
        UpdateState::Idle {
            last_checked_ms: None,
        }
    }

    /// A scripted answer for `FakeSource::check`.
    enum Script {
        NoUpdate,
        Failed(String),
        Found(Box<FakeFound>),
    }

    /// An update source that answers from a script.
    struct FakeSource(Mutex<Option<Script>>);

    impl FakeSource {
        fn new(script: Script) -> Self {
            Self(Mutex::new(Some(script)))
        }
    }

    #[async_trait::async_trait]
    impl UpdateSource for FakeSource {
        async fn check(&self) -> Result<Option<Box<dyn FoundUpdate>>, String> {
            match self.0.lock().expect("script").take() {
                Some(Script::NoUpdate) => Ok(None),
                Some(Script::Failed(message)) => Err(message),
                Some(Script::Found(found)) => Ok(Some(found as Box<dyn FoundUpdate>)),
                None => panic!("FakeSource was checked twice"),
            }
        }
    }

    /// A release the scripted source found: its download makes the given
    /// progress calls and then either succeeds or fails.
    struct FakeFound {
        progress: Vec<(u64, Option<u64>)>,
        error: Option<String>,
        installed: Arc<AtomicBool>,
    }

    impl FakeFound {
        fn new(progress: Vec<(u64, Option<u64>)>) -> Self {
            Self {
                progress,
                error: None,
                installed: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    #[async_trait::async_trait]
    impl FoundUpdate for FakeFound {
        fn version(&self) -> String {
            "2.5.0".to_string()
        }

        fn notes(&self) -> Option<String> {
            Some("notes".to_string())
        }

        async fn download(
            self: Box<Self>,
            mut progress: Box<dyn FnMut(u64, Option<u64>) + Send>,
        ) -> Result<Box<dyn PendingInstall>, String> {
            for &(downloaded, total) in &self.progress {
                progress(downloaded, total);
            }
            match &self.error {
                Some(error) => Err(error.clone()),
                None => Ok(Box::new(FakePending {
                    installed: Arc::clone(&self.installed),
                })),
            }
        }
    }

    /// The downloaded release of `FakeFound`; installing it flips its flag.
    struct FakePending {
        installed: Arc<AtomicBool>,
    }

    impl PendingInstall for FakePending {
        fn install(self: Box<Self>) -> Result<(), String> {
            self.installed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn downloads(states: &Arc<Mutex<Vec<UpdateState>>>) -> usize {
        states
            .lock()
            .expect("recorded states")
            .iter()
            .filter(|state| matches!(state, UpdateState::Downloading { .. }))
            .count()
    }

    #[test]
    fn state_serializes_with_a_phase_tag() {
        assert_eq!(
            serde_json::to_value(UpdateState::Ready {
                version: "2.5.0".to_string(),
                notes: None,
            })
            .expect("serializes"),
            serde_json::json!({"phase": "ready", "version": "2.5.0", "notes": null})
        );
        assert_eq!(
            serde_json::to_value(UpdateState::Downloading {
                version: "2.5.0".to_string(),
                downloaded: 7,
                total: None,
            })
            .expect("serializes"),
            serde_json::json!({
                "phase": "downloading",
                "version": "2.5.0",
                "downloaded": 7,
                "total": null,
            })
        );
        assert_eq!(
            serde_json::to_value(UpdateState::Disabled {
                reason: "source_build".to_string(),
            })
            .expect("serializes"),
            serde_json::json!({"phase": "disabled", "reason": "source_build"})
        );
        assert_eq!(
            serde_json::to_value(UpdateState::Checking).expect("serializes"),
            serde_json::json!({"phase": "checking"})
        );
    }

    #[test]
    fn restart_result_serializes() {
        assert_eq!(
            serde_json::to_value(UpdateRestart::WorkersRunning { count: 2 }).expect("serializes"),
            serde_json::json!({"result": "workers_running", "count": 2})
        );
        assert_eq!(
            serde_json::to_value(UpdateRestart::Started).expect("serializes"),
            serde_json::json!({"result": "started"})
        );
    }

    #[tokio::test]
    async fn a_check_with_no_update_goes_idle() {
        let now = 1_700_000_000_000;
        let (service, states) = fresh(idle());
        assert!(service.begin_check());
        service
            .run_check(&FakeSource::new(Script::NoUpdate), now)
            .await;

        assert_eq!(
            service.state(),
            UpdateState::Idle {
                last_checked_ms: Some(now),
            }
        );
        assert_eq!(
            *states.lock().expect("recorded states"),
            vec![
                UpdateState::Checking,
                UpdateState::Idle {
                    last_checked_ms: Some(now),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_found_update_downloads_and_becomes_ready() {
        let now = 1_700_000_000_000;
        let found = FakeFound::new(vec![(25, Some(100)), (50, Some(100)), (100, Some(100))]);
        let installed = Arc::clone(&found.installed);
        let (service, states) = fresh(idle());
        assert!(service.begin_check());
        service
            .run_check(&FakeSource::new(Script::Found(Box::new(found))), now)
            .await;

        let ready = UpdateState::Ready {
            version: "2.5.0".to_string(),
            notes: Some("notes".to_string()),
        };
        assert_eq!(service.state(), ready);
        let recorded = states.lock().expect("recorded states").clone();
        assert_eq!(recorded.first(), Some(&UpdateState::Checking));
        assert!(
            recorded.contains(&UpdateState::Downloading {
                version: "2.5.0".to_string(),
                downloaded: 0,
                total: None,
            }),
            "the download starts at zero: {recorded:?}"
        );
        assert_eq!(recorded.last(), Some(&ready));

        assert!(service.request_install().is_ok());
        let pending = service
            .take_requested_install()
            .expect("the requested install is pending");
        pending.install().expect("installs the downloaded release");
        assert!(installed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_failed_check_reports_and_allows_retry() {
        let now = 1_700_000_000_000;
        let (service, _states) = fresh(idle());
        assert!(service.begin_check());
        service
            .run_check(&FakeSource::new(Script::Failed("offline".to_string())), now)
            .await;

        assert_eq!(
            service.state(),
            UpdateState::Failed {
                message: "offline".to_string(),
                last_checked_ms: Some(now),
            }
        );
        assert!(service.begin_check(), "a failed check retries");
    }

    #[tokio::test]
    async fn a_failed_download_reports() {
        let now = 1_700_000_000_000;
        let mut found = FakeFound::new(vec![(10, Some(100))]);
        found.error = Some("disk full".to_string());
        let (service, _states) = fresh(idle());
        assert!(service.begin_check());
        service
            .run_check(&FakeSource::new(Script::Found(Box::new(found))), now)
            .await;

        match service.state() {
            UpdateState::Failed {
                message,
                last_checked_ms,
            } => {
                assert!(message.contains("2.5.0"), "{message}");
                assert!(message.contains("disk full"), "{message}");
                assert_eq!(last_checked_ms, Some(now));
            }
            other => panic!("expected a failed state, got {other:?}"),
        }
        assert!(service.request_install().is_err());
        assert!(service.take_requested_install().is_none());
    }

    #[test]
    fn begin_check_refuses_while_busy_or_ready_or_disabled() {
        let busy = [
            UpdateState::Checking,
            UpdateState::Downloading {
                version: "2.5.0".to_string(),
                downloaded: 1,
                total: None,
            },
            UpdateState::Ready {
                version: "2.5.0".to_string(),
                notes: None,
            },
            UpdateState::Disabled {
                reason: "source_build".to_string(),
            },
        ];
        for state in busy {
            let (service, _states) = fresh(state.clone());
            assert!(!service.begin_check(), "{state:?} must not start a check");
        }
        let retry = [
            idle(),
            UpdateState::Failed {
                message: "offline".to_string(),
                last_checked_ms: Some(1),
            },
        ];
        for state in retry {
            let (service, _states) = fresh(state.clone());
            assert!(service.begin_check(), "{state:?} must allow a check");
        }
    }

    #[tokio::test]
    async fn install_needs_a_ready_update_and_is_taken_once() {
        let (service, _states) = fresh(idle());
        assert!(service.request_install().is_err());
        assert!(service.take_requested_install().is_none());

        assert!(service.begin_check());
        service
            .run_check(
                &FakeSource::new(Script::Found(Box::new(FakeFound::new(vec![])))),
                1,
            )
            .await;

        assert!(
            service.take_requested_install().is_none(),
            "nothing is armed before the restart request"
        );
        assert!(service.request_install().is_ok());
        assert!(service.take_requested_install().is_some());
        assert!(
            service.take_requested_install().is_none(),
            "an armed install is taken once"
        );
    }

    #[tokio::test]
    async fn progress_emits_are_throttled() {
        // A known total: at most one emit per whole percent, on top of the
        // 0 % the download starts from.
        let found = FakeFound::new(
            (1..=1000u64)
                .map(|downloaded| (downloaded, Some(1000)))
                .collect(),
        );
        let (service, states) = fresh(idle());
        assert!(service.begin_check());
        service
            .run_check(&FakeSource::new(Script::Found(Box::new(found))), 1)
            .await;
        let emitted = downloads(&states);
        assert!(
            emitted <= 101,
            "every byte must not emit a state, only every percent: {emitted} downloads"
        );

        // No total: one emit per started megabyte, plus the empty start.
        let found = FakeFound::new(
            (1..=10u64)
                .map(|step| (step * 512 * 1024, None))
                .collect(),
        );
        let (service, states) = fresh(idle());
        assert!(service.begin_check());
        service
            .run_check(&FakeSource::new(Script::Found(Box::new(found))), 1)
            .await;
        let emitted = downloads(&states);
        assert!(
            emitted <= 6,
            "an unknown total emits per megabyte: {emitted} downloads"
        );
    }

    #[cfg(not(feature = "auto-update"))]
    #[test]
    fn source_builds_start_disabled() {
        assert_eq!(
            initial_state(),
            UpdateState::Disabled {
                reason: "source_build".to_string(),
            }
        );
    }
}
