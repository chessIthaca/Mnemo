// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Machine-wide heavy-command slots (plan d3336a8a step 3).
//!
//! Every Mnemo2 instance on a computer runs its own heavy commands (`cargo
//! build`/`test`/`clippy`, `npm test`/`build`/`check`). With no shared limit two
//! instances cheerfully start two full builds at once and the machine thrashes —
//! load-flaky tests, a desktop that stops responding. The limit is one number,
//! `[general.machine] heavy_commands` (default 2, range 1-16), and the slots are
//! OS FILE LOCKS under `~/.mnemo/run/heavy/`, one file per slot:
//!
//! * `std::fs::File::try_lock` on `slot-<i>.lock`. The lock belongs to the
//!   process holding the file open, so the OS releases it the moment that
//!   process exits — a crashed holder never wedges the machine.
//! * No daemon and no IPC: the filesystem IS the shared state, so any number of
//!   instances (and a hand-started `cargo test`) can poll it.
//! * A wait never fails a command: a directory that cannot be created or a file
//!   that cannot be opened degrades to "run without a slot". A machine limit is
//!   an optimisation shared across processes, never a new failure mode.
//!
//! Waiting is polling (every [`POLL_INTERVAL`]) rather than a queue: there is no
//! fairness to enforce among a handful of interactive builds, and a 500 ms
//! overshoot on a multi-minute build is noise.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::MachineConfig;

/// How long a waiter sleeps between attempts at the slots.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A wait shorter than this is not reported in a command's result (a 1 s
/// overshoot is noise, a 1 m 20 s wait is the user's answer to "why is this
/// slow?").
pub const REPORT_WAIT_AFTER: Duration = Duration::from_secs(2);

/// The heavy-slot directory for a config dir: `<config dir>/run/heavy`.
///
/// The `run/` folder is the machine-wide rendezvous every running instance
/// shares (plan d3336a8a); the heavy slots are one service-style entry in it,
/// so a custom config dir keeps its own slots.
pub fn default_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("run").join("heavy")
}

/// One held heavy slot.
///
/// The lock lives exactly as long as this guard: dropping it closes the slot
/// file, which releases the OS lock. That is also what a crashed process does,
/// which is why a dead holder never blocks the machine.
#[derive(Debug)]
pub struct SlotGuard {
    /// The locked slot file. Never read — its open handle IS the lock (the
    /// leading underscore keeps dead-code analysis quiet about that).
    _file: Option<File>,
    waited: Duration,
}

impl SlotGuard {
    /// A guard that holds nothing: a call running unslotted (no machine handle
    /// configured, or the slot directory is unusable).
    pub fn none() -> Self {
        Self {
            _file: None,
            waited: Duration::ZERO,
        }
    }

    /// How long the caller waited for the slot (zero when one was free at
    /// once).
    pub fn waited(&self) -> Duration {
        self.waited
    }
}

/// The machine's heavy slots, as seen by ONE command call.
///
/// Cheap to build per call: the slot count comes from the live
/// `[general.machine]` setting, so a Settings save applies to the next heavy
/// command without a registry rebuild.
pub struct HeavySlots {
    dir: PathBuf,
    slots: usize,
}

/// What one slot attempt found: the three outcomes are NOT the same thing.
///
/// `Busy` means every slot is held by another process, so waiting is right.
/// `Unusable` means the slot directory or a slot file cannot be used at all —
/// waiting on it would hang forever (finding slotDirHang), so the caller runs
/// unslotted instead.
#[derive(Debug)]
pub enum SlotAttempt {
    /// A slot was taken; the guard holds it until it drops.
    Taken(SlotGuard),
    /// Every slot is held by another process: waiting is the correct answer.
    Busy,
    /// The slot location cannot be used (the dir is a file, `slot-0.lock` is a
    /// directory, a permission hole, …): no slot can ever be taken here.
    Unusable(io::Error),
}

impl SlotAttempt {
    /// The guard of a `Taken` attempt, `None` for `Busy`/`Unusable`.
    pub fn guard(self) -> Option<SlotGuard> {
        match self {
            Self::Taken(guard) => Some(guard),
            _ => None,
        }
    }
}

/// The slot directories this process already warned about: ONE log line each
/// (finding slotDirHang), so a broken slot dir cannot flood the log at every
/// poll while every heavy command keeps running unslotted.
static WARNED_UNUSABLE: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
    std::sync::OnceLock::new();

/// Report an unusable slot directory once per process, naming the dir and the
/// error.
fn warn_unusable_once(dir: &Path, error: &io::Error) {
    let seen = WARNED_UNUSABLE.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let key = dir.display().to_string();
    let first = seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.clone());
    if first {
        eprintln!(
            "warning: machine slots at {key} are unusable ({error}); running without a slot"
        );
    }
}

impl HeavySlots {
    /// Slots under `dir` (the `run/heavy` directory), clamped to the setting's
    /// supported range so a zero or a wildly large number can never wedge or
    /// flood the machine.
    pub fn new(dir: PathBuf, slots: usize) -> Self {
        Self {
            dir,
            slots: MachineConfig::clamp_heavy_commands(slots),
        }
    }

    /// Slots under `dir` taken VERBATIM, with no clamp.
    ///
    /// The endpoint concurrency cap (`max_concurrent` in `endpoints.toml`,
    /// plan d3336a8a step 4) has its own range — validated at save time as
    /// 1-64 — so it must never be squeezed into the heavy-command setting's
    /// 1-16. Callers keep the [`MachineConfig::clamp_heavy_commands`] bounds
    /// for the heavy setting by using [`Self::new`].
    pub fn with_slots(dir: PathBuf, slots: usize) -> Self {
        Self { dir, slots }
    }

    /// The directory the slot files live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many slots this machine offers (the number the live-view line and
    /// the result note name).
    pub fn slots(&self) -> usize {
        self.slots
    }

    /// Try to take a free slot WITHOUT waiting.
    ///
    /// The three outcomes are distinct on purpose: `Busy` means another process
    /// holds the slots (wait), `Unusable` means this location can never work
    /// (run unslotted — waiting would hang forever, finding slotDirHang).
    pub fn try_take(&self) -> SlotAttempt {
        for index in 0..self.slots {
            let file = match self.open_slot(index) {
                Ok(file) => file,
                // An unusable slot directory must never fail a command: the
                // caller runs unslotted (documented on the module).
                Err(error) => return SlotAttempt::Unusable(error),
            };
            match file.try_lock() {
                Ok(()) => {
                    return SlotAttempt::Taken(SlotGuard {
                        _file: Some(file),
                        waited: Duration::ZERO,
                    })
                }
                Err(std::fs::TryLockError::WouldBlock) => continue,
                Err(std::fs::TryLockError::Error(error)) => {
                    return SlotAttempt::Unusable(error)
                }
            }
        }
        SlotAttempt::Busy
    }

    /// Wait for a free slot, polling every [`POLL_INTERVAL`].
    ///
    /// `on_wait` runs ONCE, when the first attempt finds every slot taken, and
    /// receives the number of slots — the live-view hook ("waiting for a build
    /// slot"). The returned guard carries the total wait
    /// ([`SlotGuard::waited`]) so the caller can report it in the result; the
    /// wait itself is outside every command deadline (the caller starts its
    /// clocks after this returns).
    pub async fn acquire<F>(&self, on_wait: F) -> SlotGuard
    where
        F: FnOnce(usize),
    {
        let started = Instant::now();
        let mut on_wait = Some(on_wait);
        loop {
            match self.try_take() {
                SlotAttempt::Taken(mut slot) => {
                    if on_wait.is_none() {
                        // Every attempt before this one found the slots taken.
                        slot.waited = started.elapsed();
                    }
                    return slot;
                }
                SlotAttempt::Unusable(error) => {
                    // Waiting on a location that can never yield a slot would
                    // hang the command forever (finding slotDirHang): say so
                    // once and run unslotted — the limit is an optimisation,
                    // never a new failure mode.
                    warn_unusable_once(&self.dir, &error);
                    return SlotGuard::none();
                }
                SlotAttempt::Busy => {}
            }
            if let Some(hook) = on_wait.take() {
                hook(self.slots);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Open (creating if needed) one slot file for locking.
    fn open_slot(&self, index: usize) -> io::Result<File> {
        std::fs::create_dir_all(&self.dir)?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join(format!("slot-{index}.lock")))
    }
}

/// The result note for a slot wait, e.g. `NOTE: waited 1m20s for a build slot.`
///
/// `None` for a wait under [`REPORT_WAIT_AFTER`] — a call that lost no
/// meaningful time says nothing.
pub fn wait_note(waited: Duration) -> Option<String> {
    (waited >= REPORT_WAIT_AFTER)
        .then(|| format!("NOTE: waited {} for a build slot.", human_duration(waited)))
}

/// A compact human duration: `45s`, `1m20s`, `12m`.
fn human_duration(waited: Duration) -> String {
    let secs = waited.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let (minutes, rest) = (secs / 60, secs % 60);
    if rest == 0 {
        format!("{minutes}m")
    } else {
        format!("{minutes}m{rest}s")
    }
}

/// Whether `command` spends one of the machine's heavy slots.
///
/// Heavy means: one of the session's own check commands (exactly as
/// `.coding/checks.toml` lists it), or a build-shaped command — `cargo`
/// followed by `build`/`test`/`clippy`/`check`/`doc`/`bench`, `npm`/`npx`/
/// `pnpm`/`yarn` followed by `test`/`vitest`/`tsc`, by `run` + a
/// `build*`/`test*`/`check*` script, or — those two run scripts WITHOUT `run` —
/// by a bare `build*`/`test*`/`check*` script name, or `vitest`/`tsc`
/// themselves.
///
/// The command line is split on `;`, `|`, `&` and newlines first, so
/// `git status; cargo test` counts — the segment that costs the minutes wins.
/// Flags are skipped when looking for the subcommand (`cargo --workspace test`
/// is a test run).
pub fn is_heavy(command: &str, check_commands: &[String]) -> bool {
    let trimmed = command.trim();
    if check_commands
        .iter()
        .any(|check| check.trim().eq_ignore_ascii_case(trimmed))
    {
        return true;
    }
    trimmed.split([';', '|', '&', '\n']).any(segment_is_heavy)
}

/// The sub-commands of `cargo` that build, test, lint or document.
const HEAVY_CARGO: [&str; 6] = ["build", "test", "clippy", "check", "doc", "bench"];

/// Package-manager flags whose NEXT token is a value, not a script name:
/// `npm --prefix ./x test` is a test run, not a run of a script called `./x`.
const NPM_VALUE_FLAGS: [&str; 3] = ["--prefix", "-C", "--workspace"];

/// The positional arguments of `program`, with the flags (and the VALUES of
/// value-taking package-manager flags) removed.
///
/// Three shapes the plain "skip tokens starting with `-`" rule got wrong
/// (finding isHeavyToolchain): `cargo +nightly test` (a `+` toolchain
/// selector), `npm --prefix ./x test` / `pnpm -C app test` / `yarn --workspace
/// app test` (the flag's value would otherwise read as the script).
fn subcommand_args<'a>(program: &str, words: impl Iterator<Item = &'a str>) -> Vec<String> {
    let package_manager = matches!(program, "npm" | "npx" | "pnpm" | "yarn");
    let mut args = Vec::new();
    let mut skip_value = false;
    for word in words {
        let word = word.trim_matches(['"', '\'']);
        if skip_value {
            skip_value = false;
            continue;
        }
        if word.starts_with('-') {
            // `--flag=value` carries its own value.
            if !word.contains('=') && package_manager && NPM_VALUE_FLAGS.contains(&word) {
                skip_value = true;
            }
            continue;
        }
        if word.starts_with('+') {
            // `cargo +nightly test`: a toolchain selector, not a subcommand.
            continue;
        }
        args.push(word.to_string());
    }
    args
}

/// Whether one shell segment (`a; b` split) is a heavy command.
fn segment_is_heavy(segment: &str) -> bool {
    let mut words = segment
        .split_whitespace()
        .filter(|word| !is_env_assignment(word));
    let Some(program) = words.next() else {
        return false;
    };
    let program = program_name(program);
    let args = subcommand_args(&program, words);
    match program.as_str() {
        "cargo" => args
            .first()
            .is_some_and(|arg| HEAVY_CARGO.contains(&arg.as_str())),
        "npm" | "npx" | "pnpm" | "yarn" => match args.first().map(String::as_str) {
            Some("test" | "vitest" | "tsc") => true,
            Some("run") => script_is_heavy(args.get(1).map(String::as_str)),
            // `yarn build` / `pnpm test`: those two run a script WITHOUT `run`
            // (finding machineDocAndShorthand), so the bare script name is as
            // heavy as `run build`.
            Some(script) if matches!(program.as_str(), "pnpm" | "yarn") => {
                script_is_heavy(Some(script))
            }
            _ => false,
        },
        "vitest" | "tsc" => true,
        _ => false,
    }
}

/// Whether a package script name is a build/test/lint-shaped run: the
/// `build*`/`test*`/`check*` families, prefixes included (`build:mnemo2`).
fn script_is_heavy(script: Option<&str>) -> bool {
    script.is_some_and(|script| {
        script.starts_with("build") || script.starts_with("test") || script.starts_with("check")
    })
}

/// The program name of a path-shaped token: basename, no `.exe`/`.cmd`/`.bat`,
/// lowercased (`C:\...\cargo.exe` ⇒ `cargo`, and PowerShell's
/// case-insensitive commands match).
fn program_name(token: &str) -> String {
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .trim_matches(['"', '\'']);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".cmd"))
        .or_else(|| base.strip_suffix(".bat"))
        .unwrap_or(base);
    base.to_ascii_lowercase()
}

/// Whether a token is a leading environment assignment (`RUSTC_WRAPPER=sccache
/// cargo build`) rather than the program.
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) if !name.is_empty() => name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    /// The setting's upper bound is what keeps a hand-edited config sane.
    #[test]
    fn slot_count_is_clamped() {
        assert_eq!(HeavySlots::new(PathBuf::from("."), 0).slots(), 1);
        assert_eq!(HeavySlots::new(PathBuf::from("."), 1000).slots(), 16);
        assert_eq!(HeavySlots::new(PathBuf::from("."), 2).slots(), 2);
    }

    /// The wait note stays quiet for a short wait and names a long one.
    #[test]
    fn wait_note_reports_only_real_waits() {
        assert!(
            wait_note(Duration::from_millis(500)).is_none(),
            "a sub-2s wait is not worth a note"
        );
        assert_eq!(
            wait_note(Duration::from_secs(80)).as_deref(),
            Some("NOTE: waited 1m20s for a build slot.")
        );
        assert_eq!(
            wait_note(Duration::from_secs(120)).as_deref(),
            Some("NOTE: waited 2m for a build slot.")
        );
    }

    /// The classification table: what counts as heavy and what never does.
    #[test]
    fn is_heavy_table() {
        let checks = vec!["cargo test --workspace".to_string()];
        for heavy in [
            "cargo build",
            "cargo test --workspace",
            "cargo --workspace test",
            "cargo +nightly test",
            "cargo +nightly-aarch64-apple-darwin build",
            "cargo clippy --workspace --all-targets",
            "cargo check",
            "cargo doc",
            "cargo bench",
            "C:\\Users\\me\\.cargo\\bin\\cargo.exe test",
            "RUSTC_WRAPPER=sccache cargo build",
            "npm test",
            "npm run build",
            "npm run check",
            "npm run build:mnemo2",
            "npm --prefix ./mnemo2 test",
            "npm --prefix=./mnemo2 test",
            "pnpm -C packages/app test",
            "pnpm --workspace app test",
            "yarn --workspace app run build",
            "npm --workspace app run build",
            "npx vitest run",
            "npX vitest",
            "pnpm test",
            "pnpm build",
            "pnpm check",
            "yarn build",
            "yarn test",
            "yarn build:mnemo2",
            "pnpm run build",
            "yarn run check",
            "vitest",
            "tsc --noEmit",
            "git status; cargo test",
            "echo hi && npm test",
            "git status | cargo test",
            "git status && cargo test",
            "  cargo test  ",
        ] {
            assert!(is_heavy(heavy, &checks), "{heavy} must be heavy");
        }
        for light in [
            "git status",
            "ls",
            "cargo fmt",
            "cargo run",
            "cargo metadata",
            "npm install",
            "yarn add x",
            "pnpm install",
            "npm build",
            "npm --prefix ./x install",
            "pnpm -C packages/app install",
            "cargo +nightly fmt",
            "npm ci",
            "npm run dev",
            "npx prettier --write .",
            "python -m pytest",
            "dir",
            "",
        ] {
            assert!(!is_heavy(light, &checks), "{light} must not be heavy");
        }
        // A session check command matches exactly (the check tool's own
        // comparison), including a command the shape table would not spot.
        assert!(is_heavy("cargo test --workspace", &checks));
        assert!(is_heavy("  cargo test --workspace  ", &checks));
        let make = vec!["make verify".to_string()];
        assert!(is_heavy("make verify", &make));
        assert!(
            !is_heavy("make verify --all", &make),
            "only the exact check command is heavy, not every make invocation"
        );
    }

    /// One slot: a second taker is refused while the first guard lives, and
    /// freed again when it drops.
    #[test]
    fn one_slot_admits_one_holder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let slots = HeavySlots::new(dir.path().to_path_buf(), 1);
        let first = slots.try_take().guard().expect("the only slot is free");
        assert!(
            matches!(slots.try_take(), SlotAttempt::Busy),
            "a second taker must wait for the slot"
        );
        assert!(dir.path().join("slot-0.lock").exists());
        drop(first);
        assert!(
            slots.try_take().guard().is_some(),
            "dropping the guard must release the slot"
        );
    }

    /// A slot directory that cannot exist (here: the path is a FILE) is
    /// UNUSABLE, not busy: acquiring returns at once with an unslotted guard
    /// instead of polling forever (finding slotDirHang).
    #[tokio::test]
    async fn unusable_slot_dir_degrades_instead_of_hanging() {
        let dir = tempfile::tempdir().expect("temp dir");
        let heavy = dir.path().join("heavy");
        std::fs::write(&heavy, b"not a directory").expect("a file where the slot dir goes");
        let slots = HeavySlots::new(heavy.clone(), 1);
        assert!(
            matches!(slots.try_take(), SlotAttempt::Unusable(_)),
            "a dir that cannot exist is unusable, not busy"
        );
        let announced = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&announced);
        let guard = tokio::time::timeout(
            Duration::from_secs(10),
            slots.acquire(move |count| {
                seen.store(count, Ordering::SeqCst);
            }),
        )
        .await
        .expect("an unusable slot dir must not hang");
        assert!(
            guard.waited().is_zero(),
            "an unusable dir never looked busy"
        );
        assert_eq!(
            announced.load(Ordering::SeqCst),
            0,
            "the waiting hook never fires for an unusable dir"
        );
    }

    /// A `slot-0.lock` that is a DIRECTORY cannot be locked, so the location is
    /// unusable — again no hang (finding slotDirHang).
    #[tokio::test]
    async fn slot_file_that_is_a_directory_is_unusable_not_busy() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("slot-0.lock")).expect("a directory where the slot file goes");
        let slots = HeavySlots::new(dir.path().to_path_buf(), 1);
        assert!(matches!(slots.try_take(), SlotAttempt::Unusable(_)));
        let guard = tokio::time::timeout(Duration::from_secs(10), slots.acquire(|_| {}))
            .await
            .expect("an unusable slot file must not hang forever");
        assert!(guard.waited().is_zero());
    }

    /// The child half of [`crashed_holder_releases_the_slot`]: takes slot 0,
    /// marks that it holds it, then dies WITHOUT unlocking. Inert (returns at
    /// once) unless the parent set the env var, so a normal suite run of this
    /// test does nothing.
    #[test]
    fn slot_crash_child() {
        let Some(dir) = std::env::var_os("MNEMO_TEST_SLOT_CRASH_DIR") else {
            return;
        };
        let slots = HeavySlots::new(PathBuf::from(dir), 1);
        let guard = slots
            .try_take()
            .guard()
            .expect("the child must get the slot");
        std::fs::write(slots.dir().join("held"), b"held").expect("marker");
        std::thread::sleep(Duration::from_millis(1500));
        // No unlock, no drop: the process dies holding the OS lock.
        std::mem::forget(guard);
        std::process::abort();
    }

    /// A crashed holder releases the slot through the OS, not through any
    /// cleanup this code could forget.
    #[test]
    fn crashed_holder_releases_the_slot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let slots = HeavySlots::new(dir.path().to_path_buf(), 1);
        // The child's run dir is its own slot directory.
        let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .args([
                "--exact",
                "host::machine_slots::tests::slot_crash_child",
                "--test-threads",
                "1",
            ])
            .env("MNEMO_TEST_SLOT_CRASH_DIR", dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the holder");
        // Wait until the child actually holds the lock.
        let marker = dir.path().join("held");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "the holder never took its slot");
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            matches!(slots.try_take(), SlotAttempt::Busy),
            "the live child holds the only slot"
        );
        let status = child.wait().expect("wait for the holder");
        assert!(!status.success(), "the holder must die holding the lock");
        assert!(
            slots.try_take().guard().is_some(),
            "a crashed holder must release its slot"
        );
    }

    /// The async path: a taken slot makes the caller wait, `on_wait` fires
    /// once with the slot count, and the wait is reported.
    #[tokio::test]
    async fn acquire_waits_for_the_held_slot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let slots = HeavySlots::new(dir.path().to_path_buf(), 1);
        let held = slots.try_take().guard().expect("free slot");
        let waiter = async {
            let announced = Arc::new(AtomicUsize::new(0));
            let seen = Arc::clone(&announced);
            let guard = slots
                .acquire(move |count| {
                    seen.store(count, Ordering::SeqCst);
                })
                .await;
            (announced.load(Ordering::SeqCst), guard)
        };
        // Release the slot after a couple of polls.
        let release = async {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            drop(held);
        };
        let ((count, guard), ()) = tokio::join!(waiter, release);
        assert_eq!(count, 1, "on_wait fires once with the slot count");
        assert!(
            guard.waited() >= Duration::from_millis(500),
            "the reported wait covers the polling, got {:?}",
            guard.waited()
        );
    }
}
