// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The automatic Laya check (plan 8106337a): the routing eval the app runs
//! by itself, like the indexing passes — no longer a button's job.
//!
//! The check starts when a project opens ([`crate::mnemo2::ipc`]), when the
//! Tiers top control saves a choice other than `off`
//! ([`crate::mnemo2::laya_settings::save_tiers_top_control`]), when a Laya
//! install settles ([`crate::mnemo2::laya_settings::settle_held_manager`])
//! and on the Check-again button ([`laya_check_start`]).
//! [`spawn_laya_check`] is the one way in: unforced, it silently skips
//! when routing is off, Laya is down or the record's fingerprint still
//! matches — the eval is deterministic for the hand set and Laya's
//! checkpoint ([`checkpoint_stamp`], [`route_eval::eval_fingerprint`]),
//! so a matching record stands, like an up-to-date index.
//!
//! Everything the check does shows on the indexing status line: it is the
//! tracker's third pass ([`PassKind::Laya`]) on the same
//! `mnemo2://indexing` channel as code and memory. A successful check
//! writes its record and applies its suggested gates itself (through
//! [`crate::mnemo2::laya_settings::update_routing_settings`]; a gate with
//! no suggestion stays as stored). A check where Laya mostly did not
//! answer fails visibly and writes nothing — the gates stand.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;
use tauri::State;

use mnemo::headless::Setup;
use mnemo::laya::{english_checkpoint, LayaManager};
use mnemo::memory::classifier::{Classifier, ClassifierStatus};

use crate::mnemo2::dispatch_routing::{self, RoutingMode};
use crate::mnemo2::indexing::{IndexingTracker, PassKind};
use crate::mnemo2::ipc::{settings_setup, Mnemo2State};
use crate::mnemo2::laya_settings::update_routing_settings;
use crate::mnemo2::route_eval;
use crate::mnemo2::team::TeamRuntime;

/// How long a started check waits for a starting sidecar (`Starting`,
/// `Installing`, `Downloading` may still turn `Ready`) before it fails.
const READY_WAIT: Duration = Duration::from_secs(180);
/// How often that wait re-reads the classifier status.
const READY_POLL: Duration = Duration::from_secs(1);
/// The whole check's time budget — a stuck sidecar fails the check rather
/// than pinning its pass open forever.
const CHECK_BUDGET: Duration = Duration::from_secs(15 * 60);

/// What the check can do with the classifier's live status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Readiness {
    /// The sidecar serves — run now.
    Ready,
    /// It may still come up — poll.
    Wait,
    /// Off or failed — neither wait nor run.
    Down,
}

/// Map the live classifier status to the check's move: a serving or
/// fine-tuning sidecar (it keeps serving the current checkpoint) runs now,
/// a starting/installing/downloading one is worth waiting for, and anything
/// else is down.
fn readiness(status: &ClassifierStatus) -> Readiness {
    match status {
        ClassifierStatus::Ready | ClassifierStatus::FineTuning { .. } => Readiness::Ready,
        ClassifierStatus::Starting
        | ClassifierStatus::Installing
        | ClassifierStatus::Downloading { .. } => Readiness::Wait,
        ClassifierStatus::Disabled | ClassifierStatus::Failed => Readiness::Down,
    }
}

/// Laya's side of the check's fingerprint: the English checkpoint id, then
/// `|`, then that checkpoint's fine-tune marker modified time in whole
/// seconds — a re-install or fine-tune re-runs the check. A missing marker
/// or an unreadable time stamps `0`.
pub fn checkpoint_stamp(manager: &LayaManager) -> String {
    let id = english_checkpoint().id;
    let modified = std::fs::metadata(manager.finetune_marker_path(id))
        .and_then(|file| file.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_secs());
    format!("{id}|{modified}")
}

/// One check's inputs, moved into its task.
pub struct LayaCheck {
    /// The lead's setup, engine-attached: its manager reads the brain's
    /// shared classifier status and its classifier slot is the brain's own
    /// (a standalone `Setup` reads `Disabled`/empty — the tests rely on
    /// that, and no sidecar is ever started by a check).
    pub setup: Setup,
    /// The lead's config dir — the `mnemo2.toml` the gates live in.
    pub config_dir: PathBuf,
    /// The open team, whose routing settings reload after the gates land.
    pub team: Option<Arc<TeamRuntime>>,
    /// The project's indexing status: the check reports as its `laya` pass.
    pub tracker: Arc<IndexingTracker>,
    /// Where the check's record goes (`route_eval::route_eval_path()` in
    /// production; tests pass a temp path, never the real `route_eval.json`).
    pub record_path: PathBuf,
}

/// Start a Laya check — the one way anything starts one.
///
/// The gates, in order: not `force`d and `[routing] mode = off` — nothing
/// to check; the classifier status [`Readiness::Down`] — an error only
/// when forced, otherwise a silent skip; the record's fingerprint still
/// matches — up to date, nothing shows in the UI (like an up-to-date
/// index); a check already running on the tracker — the later start
/// silently yields. Whatever passes spawns [`run_task`] as a tracked task
/// and reports through it, so the pass shows on the status line.
pub fn spawn_laya_check(check: LayaCheck, force: bool) -> Result<(), String> {
    if !force
        && dispatch_routing::load_routing_settings(&check.config_dir).mode == RoutingMode::Off
    {
        return Ok(());
    }
    let status = check
        .setup
        .laya_manager()
        .status_arc()
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    if let Readiness::Down = readiness(&status) {
        if force {
            return Err("Laya is not running: turn it on first".to_string());
        }
        return Ok(());
    }
    let fingerprint =
        route_eval::eval_fingerprint(&checkpoint_stamp(check.setup.laya_manager()));
    if !force
        && !route_eval::needs_check(
            route_eval::load_eval_record_at(&check.record_path).as_ref(),
            &fingerprint,
        )
    {
        return Ok(());
    }
    if !check.tracker.begin_laya() {
        return Ok(());
    }
    let tracker = Arc::clone(&check.tracker);
    let handle = tokio::spawn(run_task(check, fingerprint));
    tracker.add_task(handle);
    Ok(())
}

/// The check's task: wait for Laya to serve (the check may start while the
/// sidecar is still coming up — project open, install finish), then hand
/// the attached classifier to [`check_with`]. Every early exit reports a
/// failed `laya` pass, so nothing ever stalls silently on the status line.
async fn run_task(check: LayaCheck, fingerprint: String) {
    let LayaCheck {
        setup,
        config_dir,
        team,
        tracker,
        record_path,
    } = check;
    let deadline = tokio::time::Instant::now() + READY_WAIT;
    loop {
        let readiness_now = {
            let status = setup
                .laya_manager()
                .status_arc()
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            readiness(&status)
        };
        match readiness_now {
            Readiness::Ready => break,
            Readiness::Down => {
                tracker.apply(
                    PassKind::Laya,
                    &json!({"type": "failed", "error": "Laya did not start"}),
                );
                return;
            }
            Readiness::Wait => {}
        }
        if tokio::time::Instant::now() >= deadline {
            tracker.apply(
                PassKind::Laya,
                &json!({"type": "failed", "error": "Laya did not start"}),
            );
            return;
        }
        tokio::time::sleep(READY_POLL).await;
    }
    let classifier = setup
        .classifier_slot()
        .and_then(|slot| slot.read().unwrap_or_else(|poison| poison.into_inner()).clone());
    let Some(classifier) = classifier else {
        tracker.apply(
            PassKind::Laya,
            &json!({"type": "failed", "error": "Laya is not attached"}),
        );
        return;
    };
    check_with(
        classifier.as_ref(),
        &config_dir,
        team.as_deref(),
        &tracker,
        &record_path,
        &fingerprint,
    )
    .await;
}

/// Run the check against `classifier` and land its results: progress and
/// the outcome on the tracker's `laya` pass, the record stamped with
/// `fingerprint` at `record_path`, and the suggested gates applied through
/// [`update_routing_settings`] (a gate with no suggestion stays as stored —
/// exactly the old Apply rule). A failed check — a sidecar that stopped
/// answering, one that answered too little, or a run past
/// [`CHECK_BUDGET`] — reports itself and writes nothing; the gates stand.
pub(crate) async fn check_with(
    classifier: &dyn Classifier,
    config_dir: &Path,
    team: Option<&TeamRuntime>,
    tracker: &IndexingTracker,
    record_path: &Path,
    fingerprint: &str,
) {
    let on_progress = |done: usize, total: usize| {
        tracker.apply(
            PassKind::Laya,
            &json!({"type": "progress", "done": done, "total": total}),
        );
    };
    let mut record = match tokio::time::timeout(
        CHECK_BUDGET,
        route_eval::run_check(classifier, &on_progress),
    )
    .await
    {
        Ok(Ok(record)) => record,
        Ok(Err(message)) => {
            tracker.apply(PassKind::Laya, &json!({"type": "failed", "error": message}));
            return;
        }
        Err(_) => {
            tracker.apply(
                PassKind::Laya,
                &json!({"type": "failed", "error": "The Laya check took too long"}),
            );
            return;
        }
    };
    record.fingerprint = Some(fingerprint.to_string());
    if let Err(error) = route_eval::save_eval_record_at(record_path, &record) {
        tracker.apply(PassKind::Laya, &json!({"type": "failed", "error": error}));
        return;
    }
    if let Err(error) = update_routing_settings(config_dir, team, |settings| {
        route_eval::apply_suggested_gates(settings, &record)
    }) {
        tracker.apply(PassKind::Laya, &json!({"type": "failed", "error": error}));
        return;
    }
    tracker.apply(
        PassKind::Laya,
        &json!({"type": "done", "summary": route_eval::check_summary(&record)}),
    );
}

/// The Check-again button: run a check now against the open project's
/// Laya, forced — the mode, down and fingerprint gates all yield to the
/// button, and a failed start names why.
#[tauri::command]
pub async fn laya_check_start(state: State<'_, Mnemo2State>) -> Result<(), String> {
    let (_, runtime) = settings_setup(&state).await?;
    let Some(runtime) = runtime else {
        return Err("Open a project first: the check runs against its Laya".to_string());
    };
    runtime.start_laya_check(true).await
}

// ── Tests ───────────────────────────────────────────────────────────────────
//
// No test starts a sidecar or touches the network, and none reads or
// writes the real `~/.mnemo/laya/training/route_eval.json`: the record
// always lives in a tempdir. The classifier is a local stub; the spawn
// tests stand on a standalone `Setup`, whose manager reads `Disabled` and
// whose slot is empty.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use mnemo::memory::classifier::{Answer, Question};

    use crate::mnemo2::dispatch_routing::{RoutingMode, RoutingSettings};
    use crate::mnemo2::indexing::PassState;
    use crate::mnemo2::route_eval::EvalRecord;

    /// A classifier the tests drive: [`StubClassifier::perfect`] answers
    /// every hand-set text with the label the set itself expects (a vague
    /// one `routine`) at 0.9, [`StubClassifier::always`] answers one label
    /// for everything, and [`StubClassifier::dead`] answers nothing.
    struct StubClassifier {
        answers: BTreeMap<String, String>,
        fixed: Option<String>,
        dead: bool,
    }

    impl StubClassifier {
        fn perfect() -> Self {
            let cases = route_eval::parse_hand_set(route_eval::HAND_SET_JSONL)
                .expect("the shipped hand set parses");
            let answers = cases
                .into_iter()
                .map(|case| {
                    (
                        case.text,
                        case.expected.unwrap_or_else(|| "routine".to_string()),
                    )
                })
                .collect();
            StubClassifier {
                answers,
                fixed: None,
                dead: false,
            }
        }

        fn always(label: &str) -> Self {
            StubClassifier {
                answers: BTreeMap::new(),
                fixed: Some(label.to_string()),
                dead: false,
            }
        }

        fn dead() -> Self {
            StubClassifier {
                answers: BTreeMap::new(),
                fixed: None,
                dead: true,
            }
        }
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, state: &str, _question: &Question) -> Option<Answer> {
            if self.dead {
                return None;
            }
            let label = self.fixed.clone().unwrap_or_else(|| {
                self.answers
                    .get(state)
                    .cloned()
                    .unwrap_or_else(|| "routine".to_string())
            });
            Some(Answer::Choice {
                label,
                confidence: 0.9,
                probabilities: BTreeMap::new(),
            })
        }
    }

    /// A temp config dir with `[routing] mode = shadow` — and the custom
    /// `min_confidence_4` / `fallback_tier` the tests assert the check
    /// does or does not touch — plus the record path inside it.
    fn shadow_fixture(fallback_tier: &str, min_confidence_4: f64) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("config dir");
        dispatch_routing::save_routing_settings(
            dir.path(),
            &RoutingSettings {
                mode: RoutingMode::Shadow,
                fallback_tier: fallback_tier.to_string(),
                min_confidence_4,
                ..Default::default()
            },
        )
        .expect("write mnemo2.toml");
        let record_path = dir.path().join("route_eval.json");
        (dir, record_path)
    }

    #[tokio::test]
    async fn a_check_saves_the_record_and_applies_the_gates() {
        let (dir, record_path) = shadow_fixture("involved", 0.4);
        let tracker = IndexingTracker::new();
        let stub = StubClassifier::perfect();
        check_with(&stub, dir.path(), None, &tracker, &record_path, "fp").await;

        let record = route_eval::load_eval_record_at(&record_path)
            .expect("the check wrote its record");
        assert_eq!(record.fingerprint.as_deref(), Some("fp"));
        let settings = dispatch_routing::load_routing_settings(dir.path());
        assert_eq!(settings.mode, RoutingMode::Shadow, "the mode stands");
        assert!(
            (settings.min_confidence_4 - 0.25).abs() < 1e-9,
            "the suggested gate landed: {}",
            settings.min_confidence_4
        );
        assert_eq!(
            settings.fallback_tier, "involved",
            "applying the gates touches nothing else"
        );
        let pass = tracker.snapshot().laya.expect("the pass shows");
        assert_eq!(pass.state, PassState::Done);
        assert_eq!(pass.done, pass.total, "every case was scored");
        assert!(
            pass.detail
                .as_deref()
                .unwrap_or_default()
                .contains("Confident on"),
            "the summary tells how much was routed: {:?}",
            pass.detail
        );
    }

    #[tokio::test]
    async fn a_check_with_no_suggestion_keeps_the_gates() {
        // A Laya that answers `hard` for everything is never right on the
        // trivial/routine/involved cases, so no gate reaches the accuracy
        // floor and the record suggests nothing.
        let (dir, record_path) = shadow_fixture("routine", 0.55);
        let tracker = IndexingTracker::new();
        let stub = StubClassifier::always("hard");
        check_with(&stub, dir.path(), None, &tracker, &record_path, "fp").await;

        let record = route_eval::load_eval_record_at(&record_path)
            .expect("the record is still written");
        assert_eq!(record.fingerprint.as_deref(), Some("fp"));
        assert_eq!(record.min_confidence_4, None, "nothing was suggested");
        let settings = dispatch_routing::load_routing_settings(dir.path());
        assert!(
            (settings.min_confidence_4 - 0.55).abs() < 1e-9,
            "the stored gate stands: {}",
            settings.min_confidence_4
        );
    }

    #[tokio::test]
    async fn a_dead_laya_fails_visibly_and_writes_nothing() {
        let (dir, record_path) = shadow_fixture("routine", 0.55);
        let tracker = IndexingTracker::new();
        let stub = StubClassifier::dead();
        check_with(&stub, dir.path(), None, &tracker, &record_path, "fp").await;

        let pass = tracker.snapshot().laya.expect("the failure shows");
        assert_eq!(pass.state, PassState::Failed, "a silent Laya fails the check");
        assert!(
            pass.detail
                .as_deref()
                .unwrap_or_default()
                .contains("stopped answering"),
            "the failure names the cause: {:?}",
            pass.detail
        );
        assert!(!record_path.exists(), "a failed check writes no record");
        let settings = dispatch_routing::load_routing_settings(dir.path());
        assert!(
            (settings.min_confidence_4 - 0.55).abs() < 1e-9,
            "the stored gates stand: {}",
            settings.min_confidence_4
        );
    }

    #[test]
    fn readiness_maps_every_status() {
        assert_eq!(readiness(&ClassifierStatus::Ready), Readiness::Ready);
        assert_eq!(
            readiness(&ClassifierStatus::FineTuning {
                label: "laya-en-v1".to_string()
            }),
            Readiness::Ready,
            "a fine-tuning sidecar keeps serving"
        );
        assert_eq!(readiness(&ClassifierStatus::Starting), Readiness::Wait);
        assert_eq!(readiness(&ClassifierStatus::Installing), Readiness::Wait);
        assert_eq!(
            readiness(&ClassifierStatus::Downloading {
                label: "runtime".to_string(),
                progress: 0.4
            }),
            Readiness::Wait
        );
        assert_eq!(readiness(&ClassifierStatus::Disabled), Readiness::Down);
        assert_eq!(readiness(&ClassifierStatus::Failed), Readiness::Down);
    }

    /// A standalone `Setup` on a temp config dir (its manager reads
    /// `Disabled` — nothing installed, nothing running, no sidecar ever
    /// started), a fresh tracker, and the record path inside the dir.
    fn spawn_fixture(
        mode: RoutingMode,
    ) -> (tempfile::TempDir, Setup, Arc<IndexingTracker>, PathBuf) {
        let dir = tempfile::tempdir().expect("config dir");
        dispatch_routing::save_routing_settings(
            dir.path(),
            &RoutingSettings {
                mode,
                ..Default::default()
            },
        )
        .expect("write mnemo2.toml");
        let setup = Setup::open(Some(dir.path().to_path_buf())).expect("setup");
        let tracker = IndexingTracker::new();
        let record_path = dir.path().join("route_eval.json");
        (dir, setup, tracker, record_path)
    }

    #[test]
    fn spawn_skips_when_routing_is_off_and_not_forced() {
        let (dir, setup, tracker, record_path) = spawn_fixture(RoutingMode::Off);
        let result = spawn_laya_check(
            LayaCheck {
                setup,
                config_dir: dir.path().to_path_buf(),
                team: None,
                tracker: Arc::clone(&tracker),
                record_path,
            },
            false,
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(tracker.snapshot().laya.is_none(), "routing off starts nothing");
    }

    #[test]
    fn spawn_refuses_a_forced_check_while_laya_is_down() {
        let (dir, setup, tracker, record_path) = spawn_fixture(RoutingMode::Shadow);
        let error = spawn_laya_check(
            LayaCheck {
                setup,
                config_dir: dir.path().to_path_buf(),
                team: None,
                tracker: Arc::clone(&tracker),
                record_path,
            },
            true,
        )
        .expect_err("a forced check against a down Laya is an error");
        assert!(error.contains("not running"), "{error}");
        assert!(
            tracker.snapshot().laya.is_none(),
            "a refused check starts no pass"
        );
    }

    #[test]
    fn spawn_skips_a_matching_fingerprint() {
        let (dir, setup, tracker, record_path) = spawn_fixture(RoutingMode::Shadow);
        let manager = setup.laya_manager();
        *manager
            .status_arc()
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = ClassifierStatus::Ready;
        let fingerprint = route_eval::eval_fingerprint(&checkpoint_stamp(manager));
        route_eval::save_eval_record_at(
            &record_path,
            &EvalRecord {
                date: "2026-11-05".to_string(),
                min_confidence_4: Some(0.25),
                min_confidence_3: None,
                min_confidence_2: None,
                routed_share: None,
                accuracy: None,
                fingerprint: Some(fingerprint),
            },
        )
        .expect("write the matching record");
        let result = spawn_laya_check(
            LayaCheck {
                setup,
                config_dir: dir.path().to_path_buf(),
                team: None,
                tracker: Arc::clone(&tracker),
                record_path,
            },
            false,
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(
            tracker.snapshot().laya.is_none(),
            "a matching fingerprint runs nothing, like an up-to-date index"
        );
    }
}
