// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The two proofs a change carries.
//!
//! The **regression red check** reverses only a change's source hunks in the
//! worker's own checkout and requires the plan's regression test to fail
//! without them: a test that still passes proves nothing about the fix. The
//! checkout is the warm one the worker built in, so the ignored `target/`
//! cache is what keeps the second build fast — and every path after the
//! reverse-apply restores the checkout (`git reset --hard <tip>` then
//! `git clean -ffd`, never `-x`) and verifies the restore before an outcome is
//! reported.
//!
//! The **mutation run** runs the project's mutation tool over the change's diff
//! in a copy the tool owns and returns the mutants no test caught; each
//! survivor becomes a review finding.
//!
//! Both proofs are configured from the project's `.coding/checks.toml`
//! (`[regression]`, `[mutation]`). `[regression]` falls back to the cargo
//! command for a cargo workspace; `[mutation]` has no default and must be
//! configured.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Deserialize;

#[cfg(windows)]
use mnemo::process_tree::KillOnCloseJob;

use crate::mnemo2::phase3::{git, git_error, git_value};

/// How long one run of the regression command may take. A workspace test build
/// at the tip is incremental in the worker's checkout, but a cold dependency
/// graph still needs minutes on a slow machine.
const REGRESSION_LIMIT: Duration = Duration::from_secs(20 * 60);

/// How long the mutation tool may take. It builds and tests every mutant, so
/// the run is measured in hours, not minutes.
const MUTATION_LIMIT: Duration = Duration::from_secs(3 * 60 * 60);

/// How much of a command's output a proof reports.
const OUTPUT_TAIL: usize = 1500;

/// The `[regression]` command a cargo project gets without a config table: the
/// workspace suite filtered to one test by name.
const CARGO_REGRESSION_COMMAND: &str = "cargo test --workspace -- {name}";

/// The `[regression]` `ran` text a cargo project gets without a config table:
/// cargo prints `test <name> ... ok` for a test that ran.
const CARGO_REGRESSION_RAN: &str = "{name} ... ";

/// The proof configuration read from a project's `.coding/checks.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProofConfig {
    /// The `[regression]` table, or the cargo default for a cargo workspace.
    pub regression: Option<RegressionCfg>,
    /// The `[mutation]` table. There is no default: a project opts in.
    pub mutation: Option<MutationCfg>,
}

/// The `[regression]` table: how to run one regression test by name.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RegressionCfg {
    /// The command that runs one test; `{name}` is the plan's regression test.
    pub command: String,
    /// Text that proves the test ran; `{name}` is replaced in it too.
    pub ran: String,
}

/// The `[mutation]` table: how to run the project's mutation tool.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct MutationCfg {
    /// The command; `{diff}` is a unified diff file, `{out}` an output folder.
    pub command: String,
    /// A file under `{out}` listing the missed mutants, one per line.
    pub missed: String,
}

/// The two optional tables of `.coding/checks.toml`; every other key (`commands`
/// included) is ignored, so the check allow-list keeps its own shape.
#[derive(Debug, Default, Deserialize)]
struct ProofTables {
    #[serde(default)]
    regression: Option<RegressionCfg>,
    #[serde(default)]
    mutation: Option<MutationCfg>,
}

/// Read the proof configuration from `<workspace_root>/.coding/checks.toml`.
///
/// A malformed file loses both tables and logs one line, so a broken side-car
/// never looks configured. A project without the file still gets the cargo
/// regression default when it has a `Cargo.toml` at its root.
pub fn load_proof_config(workspace_root: &Path) -> ProofConfig {
    let path = workspace_root.join(".coding").join("checks.toml");
    let mut config = ProofConfig::default();
    if let Ok(text) = std::fs::read_to_string(&path) {
        match toml::from_str::<ProofTables>(&text) {
            Ok(tables) => {
                config.regression = tables.regression;
                config.mutation = tables.mutation;
            }
            Err(error) => {
                eprintln!(
                    "Mnemo2: {} is not valid TOML, so no proof is configured: {error}",
                    path.display()
                );
                return ProofConfig::default();
            }
        }
    }
    if config.regression.is_none() && workspace_root.join("Cargo.toml").is_file() {
        config.regression = Some(RegressionCfg {
            command: CARGO_REGRESSION_COMMAND.to_string(),
            ran: CARGO_REGRESSION_RAN.to_string(),
        });
    }
    config
}

/// Whether a changed path is a test file — a file whose hunks the red check
/// keeps, so reversing the fix never removes the test it has to run.
///
/// A path is a test file when a component is `tests` or `test`, when its file
/// name contains `.test.`, `.spec.`, `_test.` or `_tests.`, or when its file
/// name is `tests.rs`. Both `/` and `\` separate components.
pub fn is_test_path(rel: &str) -> bool {
    let name = rel.rsplit(['/', '\\']).next().unwrap_or(rel);
    if name == "tests.rs"
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains("_test.")
        || name.contains("_tests.")
    {
        return true;
    }
    rel.split(['/', '\\'])
        .any(|component| component == "tests" || component == "test")
}

/// One `diff --git` section of a unified diff: its header lines, its hunks and
/// whether it is a binary section Git reports without hunks.
struct DiffSection {
    /// The `diff --git` line and the header lines it carries (modes, index,
    /// `---`/`+++`, rename lines, a `Binary files ... differ` line), each with
    /// its original line ending.
    header: Vec<String>,
    /// Each hunk: its `@@ ... @@` line and the body lines under it.
    hunks: Vec<Vec<String>>,
    /// Whether the section is a binary change (`Binary files ... differ` or
    /// `GIT binary patch`), which carries no hunks to split.
    binary: bool,
}

/// Split a unified diff into its `diff --git` sections.
///
/// Every line keeps its original ending byte for byte: `str::lines()` would
/// strip a CRLF project's `\r`, and `git apply -R` then rejects a patch whose
/// added lines lost it, so the proof would never be more than inconclusive.
fn diff_sections(diff: &str) -> Vec<DiffSection> {
    let mut sections: Vec<DiffSection> = Vec::new();
    for line in diff.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        if text.starts_with("diff --git ") {
            sections.push(DiffSection {
                header: vec![line.to_string()],
                hunks: Vec::new(),
                binary: false,
            });
            continue;
        }
        let Some(section) = sections.last_mut() else {
            // Anything before the first section is not a patch we can apply.
            continue;
        };
        if text.starts_with("@@") {
            section.hunks.push(vec![line.to_string()]);
        } else if let Some(hunk) = section.hunks.last_mut() {
            hunk.push(line.to_string());
        } else {
            if text.starts_with("Binary files ") || text.starts_with("GIT binary patch") {
                section.binary = true;
            }
            section.header.push(line.to_string());
        }
    }
    sections
}

/// The repository-relative path a diff section is about, preferring the path
/// the file has at the tip.
fn section_path(header: &[String]) -> Option<String> {
    let mut deleted = None;
    for line in header {
        if let Some(rest) = line.strip_prefix("+++ ") {
            if rest.trim() != "/dev/null" {
                return Some(plain_path(rest));
            }
        } else if let Some(rest) = line.strip_prefix("--- ") {
            if rest.trim() != "/dev/null" {
                deleted = Some(plain_path(rest));
            }
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            return Some(plain_path(rest));
        }
    }
    if deleted.is_some() {
        return deleted;
    }
    // A rename or a mode-only change carries no `---`/`+++` line: read the
    // path out of `diff --git a/<old> b/<new>` instead.
    let rest = header.first()?.strip_prefix("diff --git ")?;
    match rest.rfind(" b/") {
        Some(index) => Some(plain_path(&rest[index + 1..])),
        None => rest.split_whitespace().last().map(plain_path),
    }
}

/// Strip Git's `a/` or `b/` side prefix and the quotes Git adds to odd paths.
fn plain_path(raw: &str) -> String {
    let raw = raw.trim();
    let raw = if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    raw.strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw)
        .to_string()
}

/// The one-based line of the first `#[cfg(test)]` in a file's text.
fn cfg_test_line(text: &str) -> Option<u32> {
    text.lines()
        .position(|line| line.trim_start().starts_with("#[cfg(test)]"))
        .map(|index| index as u32 + 1)
}

/// The new-side start line of a hunk header (`@@ -a,b +c,d @@`).
fn hunk_new_start(hunk: &[String]) -> Option<u32> {
    let header = hunk.first()?;
    let new_side = header.split_whitespace().find(|piece| piece.starts_with('+'))?;
    new_side.trim_start_matches('+').split(',').next()?.parse().ok()
}

/// Keep a hunk of a section: `true` when it is a source hunk, `false` when it
/// lies in the tip file's inline test module.
fn keep_hunk(boundary: Option<u32>, hunk: &[String]) -> bool {
    match (boundary, hunk_new_start(hunk)) {
        (Some(boundary), Some(start)) => start < boundary,
        // No boundary (no `#[cfg(test)]` in the tip file, or a header we cannot
        // read) keeps the hunk: a source hunk left behind only weakens the
        // proof, while a test hunk kept would delete the test itself.
        _ => true,
    }
}

/// Reduce a `git diff --no-color --no-ext-diff -U0 <base> <tip>` to the source
/// hunks only: test files are dropped whole, hunks of a file's inline test
/// module (`#[cfg(test)]` and below in the tip file) are dropped, and binary
/// sections are dropped too — a binary change has no hunks to split and cannot
/// be reverted hunk-wise, so keeping it would fail the whole reverse-apply.
///
/// `tip_file` reads the tip's text for a repository-relative path so the
/// boundary is measured on the file as it will be after the reverse-apply,
/// not on the base.
pub fn source_hunks_patch(diff: &str, tip_file: &dyn Fn(&str) -> Option<String>) -> String {
    let mut patch = String::new();
    for section in diff_sections(diff) {
        if section.binary {
            continue;
        }
        let path = section_path(&section.header);
        if let Some(path) = path.as_deref() {
            if is_test_path(path) {
                continue;
            }
        }
        if section.hunks.is_empty() {
            // A mode-only change has nothing to split: keep it whole.
            for line in &section.header {
                patch.push_str(line);
            }
            continue;
        }
        let boundary = path
            .as_deref()
            .filter(|path| path.ends_with(".rs"))
            .and_then(tip_file)
            .and_then(|text| cfg_test_line(&text));
        let kept: Vec<&Vec<String>> = section
            .hunks
            .iter()
            .filter(|hunk| keep_hunk(boundary, hunk))
            .collect();
        if kept.is_empty() {
            continue;
        }
        for line in &section.header {
            patch.push_str(line);
        }
        for hunk in kept {
            for line in hunk {
                patch.push_str(line);
            }
        }
    }
    patch
}

/// What a red check proved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedOutcome {
    /// The regression test failed once the change's source hunks were gone.
    Red {
        /// The failing run's output tail.
        output: String,
    },
    /// The regression test still passed without the fix: it does not test it.
    NotRed {
        /// The passing run's output tail.
        output: String,
    },
    /// Nothing could be proved, and the checkout was left exactly as it was.
    Inconclusive {
        /// Why the proof stopped.
        reason: String,
    },
}

/// What one run of a proof command produced.
enum ShellRun {
    /// The command exited; `output` is stdout followed by stderr.
    Finished {
        /// Whether the command exited zero.
        success: bool,
        /// Merged stdout and stderr.
        output: String,
    },
    /// The command outlived its limit and was killed.
    TimedOut,
}

/// The platform shell that runs one proof script.
///
/// Unix: the shell leads its own process group, so [`TreeKillGuard`] can end
/// the whole group at once.
fn shell_command(script: &str) -> tokio::process::Command {
    #[cfg(windows)]
    let command = {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = tokio::process::Command::new("powershell");
        command.args(["-NoProfile", "-Command", script]);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    #[cfg(not(windows))]
    let command = {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", script]);
        command.process_group(0);
        command
    };
    command
}

/// Owns the process TREE a proof command starts, so nothing it started
/// outlives the run: a leaked `cargo`, `rustc` or test binary keeps writing
/// into the checkout after the proof restored it, or holds the checkout open
/// and blocks the merge that deletes it.
///
/// Windows: the shell is assigned to a Job Object created with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (the `mnemo` crate's
/// [`KillOnCloseJob`]) — descendants inherit the job, so closing the handle
/// this guard owns terminates every process the command started. Unix: the
/// shell leads its own process group (see [`shell_command`]) and the group is
/// killed at drop.
///
/// The kill lives in [`Drop`] so it covers every exit: the finished run, the
/// timeout, and a proof future cancelled before either. A guard that cannot
/// own the tree (job setup failed, or the child was already reaped) falls back
/// to `kill_on_drop` of the shell alone rather than failing the run.
struct TreeKillGuard {
    #[cfg(windows)]
    job: Option<KillOnCloseJob>,
    #[cfg(unix)]
    pid: u32,
}

impl TreeKillGuard {
    /// Attach to the freshly spawned shell; never fails the run.
    fn attach(child: &tokio::process::Child) -> Self {
        #[cfg(windows)]
        {
            // The raw handle exists only while the child is not reaped; right
            // after spawn it is always present. If it is not, the process is
            // already gone and there is nothing to own.
            Self {
                job: KillOnCloseJob::assign_tokio_child(child),
            }
        }
        #[cfg(unix)]
        {
            // `process_group(0)` made the shell the leader of its own group,
            // so the group id IS its pid (0 only if it is already gone).
            Self {
                pid: child.id().unwrap_or(0),
            }
        }
    }
}

impl Drop for TreeKillGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // Closing the last job handle terminates the job.
            drop(self.job.take());
        }
        #[cfg(unix)]
        {
            if self.pid != 0 {
                // The negative pid reaches the whole group. std has no signal
                // API, so the POSIX `kill` utility does the group kill; a
                // non-zero status (no such group) is the ordinary no-op case.
                let group = format!("-{}", self.pid);
                let _ = std::process::Command::new("kill")
                    .arg("-9")
                    .arg(group)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

/// Run one proof script in `cwd` through the platform shell, bounded by
/// `limit`.
///
/// The command's whole process TREE is owned by a [`TreeKillGuard`], so a run
/// that outlives `limit` — or a proof future dropped mid-run — leaves nothing
/// behind in the checkout.
async fn run_shell(script: &str, cwd: &Path, limit: Duration) -> Result<ShellRun, String> {
    let mut command = shell_command(script);
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("could not run the proof command: {error}"))?;
    let tree = TreeKillGuard::attach(&child);
    let waited = tokio::time::timeout(limit, child.wait_with_output()).await;
    // End the tree before reporting, on every path: a finished run must not
    // leave a background child behind either.
    drop(tree);
    match waited {
        Ok(Ok(output)) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok(ShellRun::Finished {
                success: output.status.success(),
                output: combined,
            })
        }
        Ok(Err(error)) => Err(format!("the proof command could not finish: {error}")),
        Err(_) => Ok(ShellRun::TimedOut),
    }
}

/// The last `limit` characters of `text`, cut on a character boundary.
fn tail(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    text.chars().skip(count - limit).collect()
}

/// Restore a checkout to `tip` and verify it: a hard reset plus `clean -ffd`
/// (never `-x`, so the ignored build cache the proof relied on survives).
async fn restore_checkout(checkout: &Path, tip: &str) -> Result<(), String> {
    let reset = git(checkout, &["reset", "--hard", tip]).await?;
    if !reset.status.success() {
        return Err(git_error(&["reset", "--hard", tip], &reset));
    }
    let clean = git(checkout, &["clean", "-ffd"]).await?;
    if !clean.status.success() {
        return Err(git_error(&["clean", "-ffd"], &clean));
    }
    let head = git_value(checkout, &["rev-parse", "HEAD"]).await?;
    if head != tip {
        return Err(format!(
            "the checkout is at {head} after the restore, not at {tip}"
        ));
    }
    let status = git_value(checkout, &["status", "--porcelain", "-uall"]).await?;
    if !status.is_empty() {
        return Err(format!(
            "the checkout is still dirty after the restore: {}",
            tail(&status, 400)
        ));
    }
    Ok(())
}

/// The restore the drop path needs, synchronously: `git reset --hard <tip>`
/// then `git clean -ffd` (never `-x`, so the ignored build cache survives).
fn restore_now(checkout: &Path, tip: &str) -> Result<(), String> {
    for args in [vec!["reset", "--hard", tip], vec!["clean", "-ffd"]] {
        let mut command = std::process::Command::new("git");
        command
            .args(&args)
            .current_dir(checkout)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command
            .output()
            .map_err(|error| format!("git {} could not run: {error}", args.join(" ")))?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    Ok(())
}

/// Puts the checkout back when the proof future is dropped before its own
/// restore ran.
///
/// The reverse-apply is followed by an await of up to [`REGRESSION_LIMIT`];
/// if the task is aborted there (Mnemo2 closing, a cancelled turn), an armed
/// guard's [`Drop`] restores the checkout synchronously, so a dropped proof
/// never leaves a worker checkout reverse-applied. Disarmed once the caller
/// restored and verified the checkout itself.
struct RestoreGuard<'a> {
    /// The checkout the proof owns.
    checkout: &'a Path,
    /// The commit the checkout must end up on.
    tip: &'a str,
    /// Whether a restore is still owed.
    armed: bool,
}

impl<'a> RestoreGuard<'a> {
    /// A disarmed guard over `checkout` at `tip`.
    fn new(checkout: &'a Path, tip: &'a str) -> Self {
        Self {
            checkout,
            tip,
            armed: false,
        }
    }

    /// Take ownership: from here a restore is owed until [`Self::disarm`].
    fn arm(&mut self) {
        self.armed = true;
    }

    /// The caller restored and verified the checkout itself.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Drop cannot await: restore synchronously, best effort, and leave a
        // trace rather than failing silently.
        if let Err(error) = restore_now(self.checkout, self.tip) {
            eprintln!(
                "Mnemo2: the cancelled proof could not restore {}: {error}",
                self.checkout.display()
            );
        }
    }
}

/// Prove the plan's regression test fails without the change's source hunks.
///
/// The checkout must sit at `tip` with a clean status when the proof starts;
/// anything else is [`RedOutcome::Inconclusive`] and the checkout is untouched.
/// From the reverse-apply on, every path restores the checkout and verifies it
/// — the normal paths through [`restore_checkout`], a dropped future through
/// the armed [`RestoreGuard`] — and a restore that cannot be verified is an
/// `Err` the caller must surface.
pub async fn prove_regression(
    checkout: PathBuf,
    base: String,
    tip: String,
    cfg: RegressionCfg,
    test: String,
) -> Result<RedOutcome, String> {
    // 1. Preconditions: the proof owns the checkout only while it is exactly at
    //    the tip; anything else is left untouched.
    let head = git_value(&checkout, &["rev-parse", "HEAD"]).await?;
    if head != tip {
        return Ok(RedOutcome::Inconclusive {
            reason: format!("the checkout is at {head}, not at the tip {tip}"),
        });
    }
    let status = git_value(&checkout, &["status", "--porcelain", "-uall"]).await?;
    if !status.is_empty() {
        return Ok(RedOutcome::Inconclusive {
            reason: format!(
                "the checkout has uncommitted changes: {}",
                tail(&status, OUTPUT_TAIL)
            ),
        });
    }
    let script = cfg.command.replace("{name}", &test);
    let ran = cfg.ran.replace("{name}", &test);

    // 2. The test must pass at the tip, or there is nothing to prove.
    match run_shell(&script, &checkout, REGRESSION_LIMIT).await? {
        ShellRun::TimedOut => {
            return Ok(RedOutcome::Inconclusive {
                reason: "the regression test did not finish within 20 minutes at the tip"
                    .to_string(),
            });
        }
        ShellRun::Finished { success, output } => {
            if !success || !output.contains(&ran) {
                return Ok(RedOutcome::Inconclusive {
                    reason: format!(
                        "the regression test does not pass at the tip, or was not found: {}",
                        tail(&output, OUTPUT_TAIL)
                    ),
                });
            }
        }
    }

    // 3. Reverse only the change's source hunks. Nothing is written into the
    //    checkout before the apply, so an early return here stays clean.
    let merge_base = git_value(&checkout, &["merge-base", &base, &tip]).await?;
    let diff_args = ["diff", "--no-color", "--no-ext-diff", "-U0", &merge_base, &tip];
    let diff_output = git(&checkout, &diff_args).await?;
    if !diff_output.status.success() {
        return Err(git_error(&diff_args, &diff_output));
    }
    let diff = String::from_utf8_lossy(&diff_output.stdout).into_owned();
    let tip_root = checkout.clone();
    let patch = tokio::task::spawn_blocking(move || {
        source_hunks_patch(&diff, &move |rel: &str| {
            std::fs::read_to_string(tip_root.join(rel)).ok()
        })
    })
    .await
    .map_err(|error| format!("the source-hunk split failed: {error}"))?;
    if patch.trim().is_empty() {
        return Ok(RedOutcome::Inconclusive {
            reason: "the change has no source hunks".to_string(),
        });
    }
    let patch_dir = tempfile::tempdir()
        .map_err(|error| format!("could not create the source patch file: {error}"))?;
    let patch_file = patch_dir.path().join("source.patch");
    let write_file = patch_file.clone();
    tokio::task::spawn_blocking(move || std::fs::write(&write_file, patch.as_bytes()))
        .await
        .map_err(|error| format!("could not write {}: {error}", patch_file.display()))?
        .map_err(|error| format!("could not write {}: {error}", patch_file.display()))?;
    let patch_arg = patch_file.to_string_lossy().into_owned();
    let apply_args = [
        "apply",
        "-R",
        "--unidiff-zero",
        "--whitespace=nowarn",
        &patch_arg,
    ];
    // From here the checkout may be modified. The guard owes a restore until
    // the verified one below disarms it, so a proof cancelled mid-run (its
    // future dropped) still puts the checkout back.
    let mut guard = RestoreGuard::new(&checkout, &tip);
    guard.arm();
    let applied = match git(&checkout, &apply_args).await {
        Ok(applied) => applied,
        Err(error) => {
            // An apply that could not even run is not an error of the proof:
            // the caller gets an outcome, restored like every other path.
            let reason = format!("the source patch could not be reverse-applied: {error}");
            return match restore_checkout(&checkout, &tip).await {
                Ok(()) => {
                    guard.disarm();
                    Ok(RedOutcome::Inconclusive { reason })
                }
                Err(error) => Err(format!("checkout could not be restored: {error}")),
            };
        }
    };
    let outcome: Result<RedOutcome, String> = if applied.status.success() {
        // 4. The test must now fail. A non-zero exit is the proof; a passing
        //    run is the finding; a passing run that no longer reports the test
        //    proves nothing either way.
        match run_shell(&script, &checkout, REGRESSION_LIMIT).await {
            Err(error) => Err(error),
            Ok(ShellRun::TimedOut) => Ok(RedOutcome::Inconclusive {
                reason: "the regression test did not finish within 20 minutes without the fix"
                    .to_string(),
            }),
            Ok(ShellRun::Finished { success, output }) => Ok(if !success {
                RedOutcome::Red {
                    output: tail(&output, OUTPUT_TAIL),
                }
            } else if output.contains(&ran) {
                RedOutcome::NotRed {
                    output: tail(&output, OUTPUT_TAIL),
                }
            } else {
                RedOutcome::Inconclusive {
                    reason: "the test did not run without the change (it may live in a file the change adds)"
                        .to_string(),
                }
            }),
        }
    } else {
        Ok(RedOutcome::Inconclusive {
            reason: format!(
                "the source patch could not be reverse-applied: {}",
                tail(&String::from_utf8_lossy(&applied.stderr), OUTPUT_TAIL)
            ),
        })
    };

    // 5. RESTORE ALWAYS, and only then report: a checkout that cannot be
    //    verified is an error the caller has to surface, not an outcome.
    match restore_checkout(&checkout, &tip).await {
        Ok(()) => {
            guard.disarm();
            outcome
        }
        Err(error) => Err(format!("checkout could not be restored: {error}")),
    }
}

/// One mutant no test caught.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mutant {
    /// Repository-relative path, or empty when the line did not parse.
    pub file: String,
    /// One-based line, or zero when the line did not parse.
    pub line: u32,
    /// The mutation description (or the whole line when it did not parse).
    pub text: String,
}

/// Parse a cargo-mutants `missed.txt`: `path:line:col: description`, one per
/// line. A line that does not parse is kept whole so the finding still names
/// the code it is about.
fn parse_missed(text: &str) -> Vec<Mutant> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(parse_mutant_line)
        .collect()
}

/// Parse one missed-mutant line.
fn parse_mutant_line(line: &str) -> Mutant {
    let malformed = || Mutant {
        file: String::new(),
        line: 0,
        text: line.to_string(),
    };
    let Some((file, rest)) = line.split_once(':') else {
        return malformed();
    };
    let Some((number, rest)) = rest.split_once(':') else {
        return malformed();
    };
    let Some((_column, text)) = rest.split_once(':') else {
        return malformed();
    };
    let Ok(number) = number.trim().parse::<u32>() else {
        return malformed();
    };
    if file.trim().is_empty() {
        return malformed();
    }
    Mutant {
        file: file.trim().to_string(),
        line: number,
        text: text.trim().to_string(),
    }
}

/// Empty `dir`, creating it, so a run never reads a previous run's output.
fn clear_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|error| format!("could not clear the old run in {}: {error}", dir.display()))?;
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))
}

/// A worker id as a single safe path component.
fn safe_component(worker_id: &str) -> String {
    worker_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Run the project's mutation tool over the change's diff and return the
/// mutants no test caught.
///
/// The tool works in the checkout, but on its own copy of the tree: the
/// checkout must be exactly clean at `tip` when the run is over, or the run is
/// an error.
pub async fn run_mutation(
    main_root: PathBuf,
    checkout: PathBuf,
    base: String,
    tip: String,
    cfg: MutationCfg,
    worker_id: String,
) -> Result<Vec<Mutant>, String> {
    // 1. The run lasts hours: refuse a checkout that is not exactly the tip, so
    //    a stale or dirty tree is never mutated.
    let head = git_value(&checkout, &["rev-parse", "HEAD"]).await?;
    if head != tip {
        return Err(format!(
            "the mutation run needs the checkout at the tip {tip}, but it is at {head}"
        ));
    }
    let status = git_value(&checkout, &["status", "--porcelain", "-uall"]).await?;
    if !status.is_empty() {
        return Err(format!(
            "the mutation run needs a clean checkout: {}",
            tail(&status, OUTPUT_TAIL)
        ));
    }

    // 2. The diff the tool is limited to, in a folder this run owns.
    let out = main_root
        .join(".coding")
        .join("tmp")
        .join("mutation")
        .join(safe_component(&worker_id));
    let prepare = out.clone();
    tokio::task::spawn_blocking(move || clear_dir(&prepare))
        .await
        .map_err(|error| format!("the run folder could not be prepared: {error}"))??;
    let merge_base = git_value(&checkout, &["merge-base", &base, &tip]).await?;
    let diff_args = ["diff", "--no-color", &merge_base, &tip];
    let diff_output = git(&checkout, &diff_args).await?;
    if !diff_output.status.success() {
        return Err(git_error(&diff_args, &diff_output));
    }
    let diff_file = out.join("change.diff");
    let write_file = diff_file.clone();
    let diff_bytes = diff_output.stdout;
    tokio::task::spawn_blocking(move || std::fs::write(&write_file, &diff_bytes))
        .await
        .map_err(|error| format!("could not write {}: {error}", diff_file.display()))?
        .map_err(|error| format!("could not write {}: {error}", diff_file.display()))?;

    // 3. Run the tool in the checkout: it copies the tree by default, so the
    //    checkout itself is never mutated.
    let out_text = out.to_string_lossy().into_owned();
    let script = cfg
        .command
        .replace("{diff}", &diff_file.to_string_lossy())
        .replace("{out}", &out_text);
    let missed_file = cfg.missed.replace("{out}", &out_text);
    let run = run_shell(&script, &checkout, MUTATION_LIMIT).await?;
    let output_tail = match &run {
        ShellRun::Finished { output, .. } => tail(output, OUTPUT_TAIL),
        ShellRun::TimedOut => "the run was killed after 3 hours".to_string(),
    };

    // 4. Read the missed mutants.
    let missed = match &run {
        ShellRun::Finished { .. } => {
            let read_file = missed_file.clone();
            let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(&read_file))
                .await
                .map_err(|error| format!("the missed-mutant file could not be read: {error}"))?;
            read.map_err(|error| {
                format!("the mutation run wrote no missed-mutant file at {missed_file}: {error}\n{output_tail}")
            })?
        }
        ShellRun::TimedOut => {
            return Err(format!(
                "the mutation run did not finish within 3 hours\n{output_tail}"
            ));
        }
    };

    // 5. The checkout must be exactly where it was: the tool copies the tree,
    //    so anything else means the run reached outside its copy.
    let head = git_value(&checkout, &["rev-parse", "HEAD"]).await?;
    let status = git_value(&checkout, &["status", "--porcelain", "-uall"]).await?;
    if head != tip || !status.is_empty() {
        return Err(format!(
            "the mutation run changed the checkout: HEAD {head} instead of {tip}{}\n{output_tail}",
            if status.is_empty() {
                String::new()
            } else {
                format!(", status: {}", tail(&status, OUTPUT_TAIL))
            }
        ));
    }
    Ok(parse_missed(&missed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run Git in `root`, returning its trimmed stdout.
    fn git_in(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git process");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A git repository whose base commit holds `broken` and whose tip fixes it
    /// and adds a regression test. Returns the base and tip commit ids.
    fn demo_repo(root: &Path) -> (String, String) {
        demo_repo_with_eol(root, "\n")
    }

    /// [`demo_repo`] with an explicit line ending, so a CRLF project can be
    /// reproduced byte for byte.
    fn demo_repo_with_eol(root: &Path, eol: &str) -> (String, String) {
        git_in(root, &["init", "-b", "wt/demo"]);
        git_in(root, &["config", "user.name", "Proof Test"]);
        git_in(root, &["config", "user.email", "proof@example.invalid"]);
        // Deterministic bytes: the proof reverts worktree content verbatim.
        git_in(root, &["config", "core.autocrlf", "false"]);
        std::fs::write(root.join(".gitignore"), format!("target/{eol}")).expect("gitignore");
        std::fs::write(root.join("src.txt"), format!("broken{eol}")).expect("base source");
        git_in(root, &["add", ".gitignore", "src.txt"]);
        git_in(root, &["commit", "-m", "base"]);
        let base = git_in(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("src.txt"), format!("fixed{eol}")).expect("fixed source");
        std::fs::create_dir_all(root.join("tests")).expect("tests dir");
        std::fs::write(root.join("tests/demo.txt"), format!("demo{eol}")).expect("regression test");
        git_in(root, &["add", "src.txt", "tests/demo.txt"]);
        git_in(root, &["commit", "-m", "tip"]);
        let tip = git_in(root, &["rev-parse", "HEAD"]);
        (base, tip)
    }

    /// A git repository whose tip changes `lib.rs` both above `#[cfg(test)]`
    /// and inside the test module. Returns the base and tip commit ids plus the
    /// tip text.
    fn mixed_repo(root: &Path) -> (String, String, String) {
        git_in(root, &["init", "-b", "wt/demo"]);
        git_in(root, &["config", "user.name", "Proof Test"]);
        git_in(root, &["config", "user.email", "proof@example.invalid"]);
        git_in(root, &["config", "core.autocrlf", "false"]);
        let base_text = "pub fn value() -> u32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    // marker-base\n}\n";
        let tip_text = "pub fn value() -> u32 {\n    2\n}\n\n#[cfg(test)]\nmod tests {\n    // marker-tip\n}\n";
        std::fs::write(root.join("lib.rs"), base_text).expect("base lib");
        git_in(root, &["add", "lib.rs"]);
        git_in(root, &["commit", "-m", "base"]);
        let base = git_in(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("lib.rs"), tip_text).expect("tip lib");
        git_in(root, &["add", "lib.rs"]);
        git_in(root, &["commit", "-m", "tip"]);
        let tip = git_in(root, &["rev-parse", "HEAD"]);
        (base, tip, tip_text.to_string())
    }

    /// The regression script of [`mixed_repo`]: it passes when the source line
    /// still holds the fix, or when the test module was clobbered (which is the
    /// wrong split), and only fails when the source reverted and the added test
    /// line survived — exactly what a correct source-only revert produces.
    fn mixed_script() -> String {
        if cfg!(windows) {
            "$text = Get-Content lib.rs -Raw; if ($text -like '*marker-base*' -or $text -match '(?m)^\\s*2\\s*$') { 'demo ... ok' } else { 'demo ... FAILED'; exit 1 }"
                .to_string()
        } else {
            "if grep -q 'marker-base' lib.rs || grep -qE '^[[:space:]]*2[[:space:]]*$' lib.rs; then echo 'demo ... ok'; else echo 'demo ... FAILED'; exit 1; fi"
                .to_string()
        }
    }

    /// The regression script: it passes only while `src.txt` holds the fix.
    fn red_script() -> String {
        if cfg!(windows) {
            "if ((Get-Content src.txt) -eq 'fixed') { 'demo ... ok' } else { 'demo ... FAILED'; exit 1 }"
                .to_string()
        } else {
            "if [ \"$(cat src.txt)\" = fixed ]; then echo 'demo ... ok'; else echo 'demo ... FAILED'; exit 1; fi"
                .to_string()
        }
    }

    /// A regression script that passes whatever `src.txt` holds.
    fn blind_script() -> String {
        if cfg!(windows) {
            "'demo ... ok'".to_string()
        } else {
            "echo 'demo ... ok'".to_string()
        }
    }

    /// A regression script that keeps exiting zero but stops reporting the test
    /// once the source is reverted.
    fn silent_script() -> String {
        if cfg!(windows) {
            "if ((Get-Content src.txt) -eq 'fixed') { 'demo ... ok' } else { 'demo skipped' }"
                .to_string()
        } else {
            "if [ \"$(cat src.txt)\" = fixed ]; then echo 'demo ... ok'; else echo 'demo skipped'; fi"
                .to_string()
        }
    }

    /// A regression script that passes and then dirties the source file, so the
    /// reverse apply cannot find what it has to revert.
    fn scribbling_script() -> String {
        if cfg!(windows) {
            "if ((Get-Content src.txt) -eq 'fixed') { 'demo ... ok'; Set-Content src.txt 'scribbled' } else { 'demo ... FAILED'; exit 1 }"
                .to_string()
        } else {
            "if [ \"$(cat src.txt)\" = fixed ]; then echo 'demo ... ok'; echo scribbled > src.txt; else echo 'demo ... FAILED'; exit 1; fi"
                .to_string()
        }
    }

    /// A regression script that hangs on its second run, so the proof is still
    /// in flight when the caller drops its future.
    fn slow_second_run_script() -> String {
        if cfg!(windows) {
            "if ((Get-Content src.txt) -eq 'fixed') { 'demo ... ok' } else { Start-Sleep -Seconds 60; 'demo ... FAILED'; exit 1 }"
                .to_string()
        } else {
            "if [ \"$(cat src.txt)\" = fixed ]; then echo 'demo ... ok'; else sleep 60; echo 'demo ... FAILED'; exit 1; fi"
                .to_string()
        }
    }

    /// The regression script for a CRLF project: the Unix leg strips the `\r`
    /// `cat` leaves behind, exactly as a real test would have to.
    fn crlf_red_script() -> String {
        if cfg!(windows) {
            "if ((Get-Content src.txt) -eq 'fixed') { 'demo ... ok' } else { 'demo ... FAILED'; exit 1 }"
                .to_string()
        } else {
            "if [ \"$(tr -d '\\r' < src.txt)\" = fixed ]; then echo 'demo ... ok'; else echo 'demo ... FAILED'; exit 1; fi"
                .to_string()
        }
    }

    /// The proof configuration around one script.
    fn cfg_for(script: String) -> RegressionCfg {
        RegressionCfg {
            command: script,
            ran: "{name} ... ".to_string(),
        }
    }

    #[test]
    fn is_test_path_table() {
        assert!(is_test_path("src/memory/indexer/tests/openspec.rs"));
        assert!(is_test_path("mnemo2/src/lib/x.test.ts"));
        assert!(is_test_path("src-tauri/src/mnemo2/team/tests.rs"));
        assert!(is_test_path("a\\tests\\b.rs"));
        assert!(!is_test_path("src/host/brain.rs"));
        assert!(!is_test_path("src/latest.rs"));
    }

    #[test]
    fn source_patch_drops_inline_test_module_hunks() {
        let diff = concat!(
            "diff --git a/src/host/brain.rs b/src/host/brain.rs\n",
            "index 1111111..2222222 100644\n",
            "--- a/src/host/brain.rs\n",
            "+++ b/src/host/brain.rs\n",
            "@@ -2,0 +3,1 @@\n",
            "+let added = 1;\n",
            "@@ -20,0 +22,1 @@\n",
            "+let test_only = 2;\n",
        );
        let tip = (1..=25)
            .map(|line| {
                if line == 10 {
                    "#[cfg(test)]".to_string()
                } else {
                    format!("// line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let patch = source_hunks_patch(diff, &|rel| {
            if rel == "src/host/brain.rs" {
                Some(tip.clone())
            } else {
                None
            }
        });
        assert!(patch.contains("--- a/src/host/brain.rs"), "{patch}");
        assert!(patch.contains("+++ b/src/host/brain.rs"), "{patch}");
        assert!(patch.contains("+let added = 1;"), "{patch}");
        assert!(!patch.contains("test_only"), "{patch}");
    }

    #[test]
    fn source_patch_drops_test_files_and_binaries() {
        let diff = concat!(
            "diff --git a/src/memory/indexer/tests/openspec.rs ",
            "b/src/memory/indexer/tests/openspec.rs\n",
            "index 1111111..2222222 100644\n",
            "--- a/src/memory/indexer/tests/openspec.rs\n",
            "+++ b/src/memory/indexer/tests/openspec.rs\n",
            "@@ -1,0 +2,1 @@\n",
            "+fn added_test() {}\n",
            "diff --git a/assets/logo.png b/assets/logo.png\n",
            "index 3333333..4444444 100644\n",
            "Binary files a/assets/logo.png and b/assets/logo.png differ\n",
            "diff --git a/src/host/brain.rs b/src/host/brain.rs\n",
            "index 5555555..6666666 100644\n",
            "--- a/src/host/brain.rs\n",
            "+++ b/src/host/brain.rs\n",
            "@@ -4,0 +5,1 @@\n",
            "+let source = 3;\n",
        );
        let patch = source_hunks_patch(diff, &|_| None);
        assert!(!patch.contains("openspec.rs"), "{patch}");
        assert!(!patch.contains("added_test"), "{patch}");
        // A binary change has no hunks to split and cannot be reverted
        // hunk-wise: keeping it would fail the whole reverse-apply.
        assert!(!patch.contains("assets/logo.png"), "{patch}");
        assert!(!patch.contains("Binary files"), "{patch}");
        assert!(patch.contains("+let source = 3;"), "{patch}");
    }

    #[test]
    fn parse_missed_mutants() {
        let missed = concat!(
            "src/memory/indexer.rs:12:5: replace + with - in add\n",
            "mnemo2/src/lib/x.ts:3:1: delete function foo\n",
            "not a mutant line\n",
        );
        let mutants = parse_missed(missed);
        assert_eq!(
            mutants,
            vec![
                Mutant {
                    file: "src/memory/indexer.rs".to_string(),
                    line: 12,
                    text: "replace + with - in add".to_string(),
                },
                Mutant {
                    file: "mnemo2/src/lib/x.ts".to_string(),
                    line: 3,
                    text: "delete function foo".to_string(),
                },
                Mutant {
                    file: String::new(),
                    line: 0,
                    text: "not a mutant line".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn prove_regression_red_on_real_repo() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo(&root);
        std::fs::create_dir_all(root.join("target")).expect("target dir");
        std::fs::write(root.join("target/warm.txt"), "warm\n").expect("warm cache");
        let outcome = prove_regression(root.clone(), base, tip.clone(), cfg_for(red_script()), "demo".to_string())
            .await
            .expect("proof");
        match outcome {
            RedOutcome::Red { output } => assert!(output.contains("FAILED"), "{output}"),
            other => panic!("expected Red, got {other:?}"),
        }
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
        assert_eq!(
            std::fs::read_to_string(root.join("src.txt")).expect("source"),
            "fixed\n"
        );
        assert!(
            root.join("tests/demo.txt").exists(),
            "the regression test survives the reverse apply"
        );
        assert!(
            root.join("target/warm.txt").exists(),
            "the ignored build cache survives the restore"
        );
    }

    #[tokio::test]
    async fn prove_regression_not_red_when_test_ignores_the_fix() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo(&root);
        let outcome = prove_regression(
            root.clone(),
            base,
            tip.clone(),
            cfg_for(blind_script()),
            "demo".to_string(),
        )
        .await
        .expect("proof");
        match outcome {
            RedOutcome::NotRed { output } => assert!(output.contains("demo ... ok"), "{output}"),
            other => panic!("expected NotRed, got {other:?}"),
        }
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
        assert_eq!(
            std::fs::read_to_string(root.join("src.txt")).expect("source"),
            "fixed\n"
        );
    }

    #[tokio::test]
    async fn prove_regression_restores_after_an_inconclusive_second_run() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo(&root);
        let outcome = prove_regression(
            root.clone(),
            base,
            tip.clone(),
            cfg_for(silent_script()),
            "demo".to_string(),
        )
        .await
        .expect("proof");
        match outcome {
            RedOutcome::Inconclusive { reason } => {
                assert!(reason.contains("did not run"), "{reason}");
            }
            other => panic!("expected Inconclusive, got {other:?}"),
        }
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
    }

    #[tokio::test]
    async fn prove_regression_restores_after_a_failed_reverse_apply() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo(&root);
        // The test's own side effect is what breaks the reverse apply: the
        // proof must still put the checkout back at the tip afterwards.
        let outcome = prove_regression(
            root.clone(),
            base,
            tip.clone(),
            cfg_for(scribbling_script()),
            "demo".to_string(),
        )
        .await
        .expect("proof");
        match outcome {
            RedOutcome::Inconclusive { reason } => {
                assert!(reason.contains("reverse-applied"), "{reason}");
            }
            other => panic!("expected Inconclusive, got {other:?}"),
        }
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
        assert_eq!(
            std::fs::read_to_string(root.join("src.txt")).expect("source"),
            "fixed\n"
        );
    }

    #[tokio::test]
    async fn prove_regression_refuses_a_dirty_checkout() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo(&root);
        std::fs::write(root.join("scratch.txt"), "work in progress\n").expect("untracked file");
        let outcome = prove_regression(
            root.clone(),
            base,
            tip.clone(),
            cfg_for(red_script()),
            "demo".to_string(),
        )
        .await
        .expect("proof");
        assert!(
            matches!(outcome, RedOutcome::Inconclusive { .. }),
            "{outcome:?}"
        );
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(
            std::fs::read_to_string(root.join("scratch.txt")).expect("untracked file"),
            "work in progress\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src.txt")).expect("source"),
            "fixed\n"
        );
    }

    #[tokio::test]
    async fn prove_regression_mixed_file_reverts_only_source() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip, tip_text) = mixed_repo(&root);
        let outcome = prove_regression(
            root.clone(),
            base,
            tip.clone(),
            cfg_for(mixed_script()),
            "demo".to_string(),
        )
        .await
        .expect("proof");
        // Red only when the split was right: the source hunk was reverted AND
        // the added test line survived. A proof that reverted the test hunk too
        // (or dropped the source hunk) lands on NotRed and fails here.
        match outcome {
            RedOutcome::Red { output } => assert!(output.contains("FAILED"), "{output}"),
            other => panic!("expected Red, got {other:?}"),
        }
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
        assert_eq!(
            std::fs::read_to_string(root.join("lib.rs")).expect("lib"),
            tip_text
        );
    }

    #[tokio::test]
    async fn crlf_repo_proves_red() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo_with_eol(&root, "\r\n");
        let outcome = prove_regression(
            root.clone(),
            base,
            tip.clone(),
            cfg_for(crlf_red_script()),
            "demo".to_string(),
        )
        .await
        .expect("proof");
        // A patch that lost its `\r` cannot be reverse-applied, so the proof
        // would stop at Inconclusive instead of proving red.
        match outcome {
            RedOutcome::Red { output } => assert!(output.contains("FAILED"), "{output}"),
            other => panic!("expected Red, got {other:?}"),
        }
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
        assert_eq!(
            std::fs::read_to_string(root.join("src.txt")).expect("source"),
            "fixed\r\n"
        );
    }

    #[tokio::test]
    async fn restore_checkout_restores_the_tip_and_keeps_ignored_files() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (_base, tip) = demo_repo(&root);
        std::fs::create_dir_all(root.join("target")).expect("target dir");
        std::fs::write(root.join("target/warm.txt"), "warm\n").expect("warm cache");
        std::fs::write(root.join("src.txt"), "reverted\n").expect("dirty source");
        std::fs::write(root.join("scratch.txt"), "leftover\n").expect("untracked file");
        restore_checkout(&root, &tip).await.expect("restore");
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
        assert_eq!(
            std::fs::read_to_string(root.join("src.txt")).expect("source"),
            "fixed\n"
        );
        assert!(
            root.join("target/warm.txt").exists(),
            "`clean` must not use -x"
        );
    }

    #[tokio::test]
    async fn dropped_proof_restores_the_checkout() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo(&root);
        let proof = prove_regression(
            root.clone(),
            base,
            tip.clone(),
            cfg_for(slow_second_run_script()),
            "demo".to_string(),
        );
        // The second run hangs, so the future is dropped while the checkout is
        // reverse-applied: the guard's drop must put it back.
        let waited = tokio::time::timeout(Duration::from_secs(5), proof).await;
        assert!(
            waited.is_err(),
            "the proof must still be in flight when it is dropped"
        );
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git_in(&root, &["status", "--porcelain", "-uall"]), "");
        assert_eq!(
            std::fs::read_to_string(root.join("src.txt")).expect("source"),
            "fixed\n"
        );
    }

    #[tokio::test]
    async fn timed_out_proof_kills_child_tree() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path().to_path_buf();
        let marker = root.join("marker.txt");
        // The command starts a CHILD process of its own that writes a marker
        // after 3 s, then the command itself hangs: killing only the shell
        // would let the child write the marker and leak past the run.
        let script = if cfg!(windows) {
            let child = root.join("child.ps1");
            std::fs::write(
                &child,
                format!(
                    "Start-Sleep -Seconds 3; Set-Content -Path '{}' -Value leaked",
                    marker.display()
                ),
            )
            .expect("child script");
            format!(
                "Start-Process -WindowStyle Hidden -FilePath powershell -ArgumentList '-NoProfile','-File','{}' | Out-Null; Start-Sleep -Seconds 60",
                child.display()
            )
        } else {
            let child = root.join("child.sh");
            std::fs::write(
                &child,
                format!("sleep 3; echo leaked > '{}'", marker.display()),
            )
            .expect("child script");
            format!("sh '{}' & sleep 60", child.display())
        };
        let run = run_shell(&script, &root, Duration::from_secs(1))
            .await
            .expect("run");
        assert!(matches!(run, ShellRun::TimedOut), "the run must time out");
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            !marker.exists(),
            "a timed-out proof left a child running that wrote {}",
            marker.display()
        );
    }

    #[tokio::test]
    async fn run_mutation_refuses_a_dirty_checkout() {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path().to_path_buf();
        let (base, tip) = demo_repo(&root);
        // A command that would leave a trace if the run ever started.
        let command = if cfg!(windows) {
            "Set-Content ran.txt 'ran'"
        } else {
            "echo ran > ran.txt"
        };
        let cfg = MutationCfg {
            command: command.to_string(),
            missed: "{out}/mutants.out/missed.txt".to_string(),
        };
        std::fs::write(root.join("scratch.txt"), "work in progress\n").expect("untracked file");
        let error = run_mutation(
            root.clone(),
            root.clone(),
            base,
            tip.clone(),
            cfg,
            "worker-1".to_string(),
        )
        .await
        .expect_err("a dirty checkout must be refused");
        assert!(error.contains("clean"), "{error}");
        assert!(
            !root.join(".coding/tmp/mutation/worker-1").exists(),
            "nothing may be prepared for a refused run"
        );
        assert!(!root.join("ran.txt").exists(), "the tool must not have run");
        assert_eq!(git_in(&root, &["rev-parse", "HEAD"]), tip);
    }

    #[test]
    fn load_proof_config_defaults_for_a_cargo_project() {
        let dir = tempfile::tempdir().expect("root");
        let root = dir.path();
        assert_eq!(load_proof_config(root), ProofConfig::default());
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("cargo");
        let config = load_proof_config(root);
        assert_eq!(
            config.regression,
            Some(RegressionCfg {
                command: "cargo test --workspace -- {name}".to_string(),
                ran: "{name} ... ".to_string(),
            })
        );
        assert_eq!(config.mutation, None);
    }

    #[test]
    fn load_proof_config_reads_the_tables() {
        let dir = tempfile::tempdir().expect("root");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".coding")).expect("coding dir");
        std::fs::write(
            root.join(".coding/checks.toml"),
            concat!(
                "commands = [\"cargo test --workspace\"]\n",
                "[regression]\n",
                "command = \"cargo test --workspace -- {name}\"\n",
                "ran = \"{name} ... \"\n",
                "[mutation]\n",
                "command = \"cargo mutants --in-diff {diff} --output {out}\"\n",
                "missed = \"{out}/mutants.out/missed.txt\"\n",
            ),
        )
        .expect("checks");
        let config = load_proof_config(root);
        assert_eq!(
            config.regression,
            Some(RegressionCfg {
                command: "cargo test --workspace -- {name}".to_string(),
                ran: "{name} ... ".to_string(),
            })
        );
        assert_eq!(
            config.mutation,
            Some(MutationCfg {
                command: "cargo mutants --in-diff {diff} --output {out}".to_string(),
                missed: "{out}/mutants.out/missed.txt".to_string(),
            })
        );
    }

    #[test]
    fn load_proof_config_forgets_a_malformed_file() {
        let dir = tempfile::tempdir().expect("root");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".coding")).expect("coding dir");
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("cargo");
        std::fs::write(root.join(".coding/checks.toml"), "commands = [oops\n").expect("checks");
        assert_eq!(load_proof_config(root), ProofConfig::default());
    }
}
