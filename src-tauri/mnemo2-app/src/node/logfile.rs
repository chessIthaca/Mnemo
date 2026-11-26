// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `<home>/node.log`: everything the node prints to stderr, kept on disk.
//!
//! The node runtime and the engine report through `eprintln!`, so the log is
//! made at the process level: stderr is pointed at a pipe, and one thread
//! copies each chunk to the original stderr and each line, with a UTC
//! timestamp, to `node.log` (append). When the file would pass 5 MiB it is
//! renamed to `node.log.1` (replacing the earlier one) and a fresh file starts.

use std::fs::{File, OpenOptions};
use std::io::{PipeReader, PipeWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
type Saved = std::os::fd::OwnedFd;
#[cfg(windows)]
type Saved = std::os::windows::io::OwnedHandle;

/// The log file inside the node home.
const LOG_FILE: &str = "node.log";
/// Where the log goes when it grows past [`ROTATE_AT`].
const ROTATED_FILE: &str = "node.log.1";
/// The size at which `node.log` rotates.
const ROTATE_AT: u64 = 5 * 1024 * 1024;
/// The longest unterminated stretch the line splitter holds back.
const MAX_LINE: usize = 64 * 1024;
/// How long [`LogTee::finish`] waits for the copy thread to drain.
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// A running stderr copy; [`LogTee::finish`] puts stderr back and drains it.
pub struct LogTee {
    writer: PipeWriter,
    original: Option<Saved>,
    done: mpsc::Receiver<()>,
    thread: JoinHandle<()>,
}

/// Start copying stderr into `<home>/node.log`. A failure to set it up is
/// reported on stderr and leaves stderr as it was (`None`).
pub fn start(home: &Path) -> Option<LogTee> {
    if let Err(error) = std::fs::create_dir_all(home) {
        eprintln!("mnemo2-node: no log file ({}): {error}", home.display());
        return None;
    }
    let log = match RotatingLog::open(home) {
        Ok(log) => log,
        Err(error) => {
            eprintln!("mnemo2-node: could not open the log file: {error}");
            return None;
        }
    };
    let (reader, writer) = match std::io::pipe() {
        Ok(pipe) => pipe,
        Err(error) => {
            eprintln!("mnemo2-node: no log file (pipe): {error}");
            return None;
        }
    };
    let original = clone_stderr();
    let passthrough = original
        .as_ref()
        .and_then(|saved| saved.try_clone().ok())
        .map(File::from);
    if let Err(error) = redirect_stderr(&writer) {
        eprintln!("mnemo2-node: no log file (redirect): {error}");
        return None;
    }
    let (done_tx, done) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("mnemo2-node-log".to_string())
        .spawn(move || {
            pump(reader, passthrough, log);
            let _ = done_tx.send(());
        });
    match thread {
        Ok(thread) => Some(LogTee {
            writer,
            original,
            done,
            thread,
        }),
        Err(error) => {
            restore_stderr(original);
            eprintln!("mnemo2-node: no log file (thread): {error}");
            None
        }
    }
}

impl LogTee {
    /// Put stderr back, then wait (briefly) for the last lines to reach the
    /// file. A grandchild that still holds the pipe cannot hold the exit up.
    pub fn finish(self) {
        let Self {
            writer,
            original,
            done,
            thread,
        } = self;
        restore_stderr(original);
        drop(writer);
        if done.recv_timeout(DRAIN_WAIT).is_ok() {
            let _ = thread.join();
        }
    }
}

/// Copy `reader` to the passthrough stderr and, line by line, to the log.
fn pump(mut reader: PipeReader, mut passthrough: Option<File>, mut log: RotatingLog) {
    let mut buffer = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if let Some(out) = passthrough.as_mut() {
            let _ = out.write_all(&buffer[..read]);
        }
        pending.extend_from_slice(&buffer[..read]);
        while let Some(at) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=at).collect();
            log.write_line(trim_eol(&line));
        }
        if pending.len() > MAX_LINE {
            log.write_line(&pending);
            pending.clear();
        }
    }
    if !pending.is_empty() {
        log.write_line(trim_eol(&pending));
    }
}

/// `line` without its trailing `\n` / `\r\n`.
fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// An append-only log that rotates once at [`ROTATE_AT`].
struct RotatingLog {
    path: PathBuf,
    rotated: PathBuf,
    file: Option<File>,
    size: u64,
}

impl RotatingLog {
    fn open(home: &Path) -> std::io::Result<Self> {
        let mut log = Self {
            path: home.join(LOG_FILE),
            rotated: home.join(ROTATED_FILE),
            file: None,
            size: 0,
        };
        log.size = std::fs::metadata(&log.path).map(|meta| meta.len()).unwrap_or(0);
        if log.size >= ROTATE_AT {
            log.rotate()?;
        }
        log.file = Some(log.open_append()?);
        Ok(log)
    }

    fn open_append(&self) -> std::io::Result<File> {
        OpenOptions::new().create(true).append(true).open(&self.path)
    }

    /// Move the full log aside and start an empty one.
    fn rotate(&mut self) -> std::io::Result<()> {
        // Close first: Windows cannot rename an open file.
        self.file = None;
        std::fs::rename(&self.path, &self.rotated)?;
        self.size = 0;
        self.file = Some(self.open_append()?);
        Ok(())
    }

    fn write_line(&mut self, line: &[u8]) {
        let mut entry = utc_stamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0),
        )
        .into_bytes();
        entry.push(b' ');
        entry.extend_from_slice(line);
        entry.push(b'\n');
        if self.size + entry.len() as u64 > ROTATE_AT && self.rotate().is_err() {
            // A log that cannot rotate stops growing instead of failing the node.
            return;
        }
        if let Some(file) = self.file.as_mut() {
            if file.write_all(&entry).is_ok() {
                self.size += entry.len() as u64;
            }
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for Unix seconds (the proleptic Gregorian calendar).
fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Days since 0000-03-01, split into 400-year eras.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

// ── the platform half: stderr as a swappable handle ─────────────────────────

/// A duplicate of the current stderr, or `None` when there is none.
#[cfg(unix)]
fn clone_stderr() -> Option<Saved> {
    use std::os::fd::AsFd;
    std::io::stderr().as_fd().try_clone_to_owned().ok()
}

/// A duplicate of the current stderr, or `None` when there is none.
#[cfg(windows)]
fn clone_stderr() -> Option<Saved> {
    use std::os::windows::io::AsHandle;
    std::io::stderr().as_handle().try_clone_to_owned().ok()
}

/// Point the process's stderr at `writer`.
#[cfg(unix)]
fn redirect_stderr(writer: &PipeWriter) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: dup2 on two descriptors this process owns; descriptor 2 is
    // replaced atomically.
    let done = unsafe { libc::dup2(writer.as_raw_fd(), libc::STDERR_FILENO) };
    if done < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Point the process's stderr at `writer`.
#[cfg(windows)]
fn redirect_stderr(writer: &PipeWriter) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: the handle is a live pipe end that this process owns; std reads
    // the standard handle again on every stderr write.
    let done = unsafe { SetStdHandle(STD_ERROR_HANDLE, writer.as_raw_handle()) };
    if done == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Put the saved stderr back as the process's stderr.
#[cfg(unix)]
fn restore_stderr(original: Option<Saved>) {
    use std::os::fd::AsRawFd;
    if let Some(original) = original {
        // SAFETY: as in `redirect_stderr`; the duplicate stays valid until
        // it drops after this call.
        unsafe {
            libc::dup2(original.as_raw_fd(), libc::STDERR_FILENO);
        }
    }
}

/// Put the saved stderr back as the process's stderr.
#[cfg(windows)]
fn restore_stderr(original: Option<Saved>) {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    if let Some(original) = original {
        // The handle becomes the process's stderr again, so it must outlive
        // this function: it is handed over, not closed.
        let handle = original.into_raw_handle();
        // SAFETY: `handle` is the live duplicate of the original stderr.
        unsafe {
            SetStdHandle(STD_ERROR_HANDLE, handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_iso() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc_stamp(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn trims_line_endings() {
        assert_eq!(trim_eol(b"abc\r\n"), b"abc");
        assert_eq!(trim_eol(b"abc\n"), b"abc");
        assert_eq!(trim_eol(b"abc"), b"abc");
    }

    #[test]
    fn rotates_once_at_the_limit() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = RotatingLog::open(dir.path()).expect("open");
        let line = vec![b'x'; 1024 * 1024];
        for _ in 0..6 {
            log.write_line(&line);
        }
        let rotated = dir.path().join(ROTATED_FILE);
        assert!(rotated.is_file(), "the full log moves to node.log.1");
        assert!(std::fs::metadata(&rotated).expect("rotated").len() <= ROTATE_AT);
        let current = std::fs::metadata(dir.path().join(LOG_FILE)).expect("current");
        assert!(current.len() < ROTATE_AT);
        assert!(current.len() > 0);
    }

    #[test]
    fn an_oversized_log_rotates_when_opened() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join(LOG_FILE),
            vec![b'y'; ROTATE_AT as usize + 1],
        )
        .expect("seed");
        let _log = RotatingLog::open(dir.path()).expect("open");
        assert!(dir.path().join(ROTATED_FILE).is_file());
        assert_eq!(
            std::fs::metadata(dir.path().join(LOG_FILE)).expect("log").len(),
            0
        );
    }
}
