// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The live-settings watcher — a hand edit of `~/.mnemo/*.toml`, or another
//! instance's save, reaches a running engine without a restart.
//!
//! The engine reads its config through `Setup` and applies a Settings save
//! live ([`crate::headless::Setup::patch_settings`]). Nothing watched the files
//! themselves, so a hand edit (or a second instance's Settings dialog) waited
//! for the next engine start. This module closes that gap with an OS-level
//! watcher ([`notify::RecommendedWatcher`]) over the config directory: a
//! create/modify/remove of one of the four live config files
//! ([`CONFIG_FILE_NAMES`](crate::config::CONFIG_FILE_NAMES)) triggers ONE
//! reload once the burst goes quiet.
//!
//! Three properties are load-bearing:
//!
//! * **OS-only.** The watcher knows nothing about `Config`: the caller passes a
//!   `reload` closure and does the fingerprint check, the load and the
//!   live-apply ([`crate::headless::Setup::reload_if_changed`]). That keeps the
//!   comparison testable without the filesystem and the reload semantics with
//!   the config owner.
//! * **Coalesced.** A save writes all four files (each staged as `.tmp` and
//!   backed up as `.bak`) in quick succession; the debounce turns the burst
//!   into a single reload, and the `.tmp`/`.bak` names never count.
//! * **Bounded.** Dropping the [`ConfigWatcher`] drops the OS watch, which
//!   closes the event channel and ends the debounce task — a closed project's
//!   engine is not kept alive by a reload callback holding its handles.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc as tokio_mpsc;

use crate::config::CONFIG_FILE_NAMES;

/// How long the watcher waits for the event burst to go quiet before running
/// one reload. A save touches all four live files (plus their staged/backup
/// siblings) in quick succession and must produce one reload, not four.
pub const CONFIG_DEBOUNCE: Duration = Duration::from_millis(500);

/// The OS watcher behind the live-settings reload — see the module docs.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    _task: Option<tokio::task::JoinHandle<()>>,
}

/// Dropping the watcher cancels its debounce task right away. Closing the
/// event channel alone ends the loop only after its debounce sleep, and
/// until then the task's `reload` closure keeps everything it captured (a
/// host's live config handles, and through them the project's stores)
/// alive — and could still run a reload against a project that is closing.
impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        if let Some(task) = self._task.take() {
            task.abort();
        }
    }
}

impl ConfigWatcher {
    /// Watch `config_dir` for changes to the four live config files and call
    /// `reload` once per debounced burst.
    ///
    /// `reload` is where the caller does the work (fingerprint check, load,
    /// live-apply). It runs on the debounce task, never on the watcher's own
    /// callback thread.
    ///
    /// Returns `Err` when the OS watcher cannot be created or `config_dir`
    /// cannot be watched — the caller logs this and continues without live
    /// reloads (the next engine start still sees the files).
    pub fn spawn<F, Fut>(
        config_dir: PathBuf,
        debounce: Duration,
        reload: F,
    ) -> Result<Self, notify::Error>
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        // Bridge notify's synchronous callback into async via an unbounded
        // tokio channel: `UnboundedSender::send` is a plain sync call (no
        // await), so it is safe to use directly in the watcher callback.
        let (tx, rx) = tokio_mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            // Best-effort: if the debounce task has gone away, drop the event.
            let _ = tx.send(res);
        })?;
        // Non-recursive: the four live files are direct children of the config
        // dir, and the subtrees beside them (`laya/`, `run/`, `knowledge/`)
        // are written constantly — watching them would wake the loop for
        // nothing.
        watcher.watch(&config_dir, RecursiveMode::NonRecursive)?;

        let task_dir = config_dir.clone();
        // Run the debounce loop on a Tokio runtime. Most callers are already
        // inside one, but a host may build its runtime from a context with NO
        // reactor — a bare `tokio::spawn` panics ("no reactor running"),
        // crashing the app on startup. Fall back to a dedicated thread with
        // its own single-threaded runtime: the loop is one task that exits
        // when the channel closes (watcher dropped), so the thread terminates
        // on its own and never needs a handle.
        let task = match tokio::runtime::Handle::try_current() {
            Ok(handle) => Some(handle.spawn(async move {
                Self::debounce_loop(task_dir, rx, debounce, reload).await;
            })),
            Err(_) => {
                let thread = std::thread::Builder::new()
                    .name("config-watcher".to_string())
                    .spawn(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                eprintln!("config: watcher runtime failed to start: {e}");
                                return;
                            }
                        };
                        rt.block_on(Self::debounce_loop(task_dir, rx, debounce, reload));
                    });
                match thread {
                    Ok(_) => None,
                    Err(e) => {
                        eprintln!("config: watcher thread failed to start: {e}");
                        return Err(notify::Error::generic(&format!(
                            "could not spawn watcher thread: {e}"
                        )));
                    }
                }
            }
        };

        Ok(Self {
            _watcher: watcher,
            _task: task,
        })
    }

    /// The debounce loop: consume watcher events, coalesce a burst, then run
    /// one reload.
    async fn debounce_loop<F, Fut>(
        config_dir: PathBuf,
        mut rx: tokio_mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
        debounce: Duration,
        reload: F,
    ) where
        F: Fn() -> Fut + Send,
        Fut: std::future::Future<Output = ()> + Send,
    {
        loop {
            // Wait for the first event of a burst. `recv` returns None when
            // the watcher is dropped (channel closed) — exit the loop.
            let Some(first) = rx.recv().await else {
                return;
            };
            let mut touched = Self::event_touches_config(&first, &config_dir);

            // Coalesce the burst: keep draining until `debounce` passes with
            // no new events. Any event resets the timer so a burst produces
            // ONE reload at the end, not N.
            loop {
                tokio::time::sleep(debounce).await;
                let mut saw_more = false;
                while let Ok(event) = rx.try_recv() {
                    saw_more = true;
                    if Self::event_touches_config(&event, &config_dir) {
                        touched = true;
                    }
                }
                if !saw_more {
                    break; // quiet period — the burst is over
                }
            }

            if touched {
                reload().await;
            }
        }
    }

    /// Whether one watcher message names a live config file.
    ///
    /// A watcher error is treated as a possible change: the reload's own
    /// fingerprint check makes a spurious call free, while a missed change
    /// would strand an instance on the old settings.
    fn event_touches_config(
        event: &notify::Result<notify::Event>,
        config_dir: &Path,
    ) -> bool {
        let event = match event {
            Ok(event) => event,
            Err(_) => return true,
        };
        // Metadata-only and access events cannot change content.
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }
        event
            .paths
            .iter()
            .any(|path| is_live_config_file(path, config_dir))
    }
}

/// Whether `path` is one of the four live config files directly inside
/// `config_dir`.
///
/// The staging (`config.toml.tmp`) and backup (`config.toml.bak`) siblings a
/// save leaves beside each live file never match: only the committed names
/// count, so one save is one burst at most.
pub(crate) fn is_live_config_file(path: &Path, config_dir: &Path) -> bool {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if !CONFIG_FILE_NAMES.contains(&name) {
        return false;
    }
    match path.parent() {
        Some(parent) => parent == config_dir || same_dir(parent, config_dir),
        None => false,
    }
}

/// Whether two directory paths name the same directory — exactly, or after
/// resolving symlinks (FSEvents on macOS reports resolved paths, so a watched
/// config dir under a symlink — every macOS tempdir — never matches raw).
fn same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use notify::event::ModifyKind;
    use notify::{Event, EventKind};

    fn event(kind: EventKind, path: PathBuf) -> notify::Result<Event> {
        Ok(Event::new(kind).add_path(path))
    }

    #[test]
    fn only_the_four_live_files_at_the_top_level_count() {
        let dir = Path::new("cfg");
        for name in CONFIG_FILE_NAMES {
            assert!(
                is_live_config_file(&dir.join(name), dir),
                "{name} is a live config file"
            );
        }
        for name in [
            "projects.toml",
            "config.toml.tmp",
            "config.toml.bak",
            "keys.toml.1234",
        ] {
            assert!(
                !is_live_config_file(&dir.join(name), dir),
                "{name} must never trigger a reload"
            );
        }
        assert!(
            !is_live_config_file(&dir.join("laya").join("config.toml"), dir),
            "a nested config.toml is a different file"
        );
    }

    #[test]
    fn access_events_do_not_count() {
        let dir = Path::new("cfg");
        let access = event(EventKind::Access(notify::event::AccessKind::Read), dir.join("config.toml"));
        assert!(!ConfigWatcher::event_touches_config(&access, dir));
        let modify = event(EventKind::Modify(ModifyKind::Any), dir.join("config.toml"));
        assert!(ConfigWatcher::event_touches_config(&modify, dir));
        let failed = Err(notify::Error::generic("buffer overflow"));
        assert!(
            ConfigWatcher::event_touches_config(&failed, dir),
            "a watcher error must be re-checked, not dropped"
        );
    }

    /// The one real-watcher test: an unwatched file stays quiet, a live file
    /// wakes exactly one reload, and a burst coalesces into that one.
    #[tokio::test]
    async fn a_burst_of_live_file_writes_runs_one_reload() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let watcher = ConfigWatcher::spawn(
            dir.path().to_path_buf(),
            Duration::from_millis(150),
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .expect("the OS watcher starts on a temp dir");
        // Give the OS watch a moment to settle before the first write.
        tokio::time::sleep(Duration::from_millis(100)).await;

        std::fs::write(dir.path().join("projects.toml"), "[[project]]\n").unwrap();
        std::fs::write(dir.path().join("config.toml.tmp"), "[general]\n").unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "unwatched files must not trigger a reload"
        );

        // A save's burst: all four live files in quick succession.
        for (index, name) in CONFIG_FILE_NAMES.iter().enumerate() {
            std::fs::write(dir.path().join(name), format!("[section_{index}]\n")).unwrap();
        }
        for _ in 0..50 {
            if calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the burst must coalesce into one reload"
        );
        drop(watcher);
    }
}
