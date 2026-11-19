// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Ownership of spawned process trees.
//!
//! On Windows a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` makes the
//! operating system terminate a process and all of its descendants when the
//! last job handle closes — including when the owning process itself exits or
//! is force-ended. Unix callers use a process group and an explicit group kill;
//! parent-death cleanup on Unix is intentionally out of scope.

/// A Windows Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` that a spawned
/// process (and every descendant it starts afterwards) is assigned to. Dropping
/// it, or the owning process dying, terminates the whole tree. A no-op shell on
/// non-Windows targets.
pub struct KillOnCloseJob {
    #[cfg(windows)]
    handle: OwnedJobHandle,
}

/// Owned job-object handle. A newtype because windows-sys' `HANDLE` is a raw
/// pointer (not `Send`), while users of the job must be able to move between
/// threads (for example inside an `async_trait` tool future).
#[cfg(windows)]
struct OwnedJobHandle(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: an owned kernel handle is a plain kernel-object reference with no
// thread affinity — moving it between threads is as sound as moving the guard
// that owns it, and access stays single-threaded (kill/drop).
#[cfg(windows)]
unsafe impl Send for OwnedJobHandle {}

impl KillOnCloseJob {
    /// Create a job and assign the process behind `raw` (a child's raw handle)
    /// to it. Return `None` when creation, configuration or assignment fails
    /// (the caller falls back to killing the root only). The raw handle is only
    /// a kernel-object reference: an invalid one fails the Win32 call, so this
    /// safe crate-private function never dereferences it.
    #[cfg(windows)]
    pub(crate) fn assign_raw(raw: std::os::windows::io::RawHandle) -> Option<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Kill every process in the job when the LAST handle to it closes.
        // Descendants inherit job membership, and breakaway is not granted.
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: a plain Win32 call sequence on objects created and owned
        // here. `raw` is borrowed only for AssignProcessToJobObject; the job
        // handle is closed on every failure path and owned by Self on success.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            let assigned = configured != 0 && AssignProcessToJobObject(job, raw) != 0;
            if !assigned {
                // SAFETY: the handle comes from CreateJobObjectW above and is
                // no longer used.
                CloseHandle(job);
                return None;
            }
            Some(Self {
                handle: OwnedJobHandle(job),
            })
        }
    }

    /// Create a job and assign a spawned child process to it, so the child and
    /// every process it starts die with the returned guard. `None` means the
    /// job could not be set up and the caller should fall back to killing the
    /// root process only. Public for the app crate's Mnemo2 team, which owns the
    /// process tree of a proof command; it takes a safe child handle, never a
    /// raw pointer.
    #[cfg(windows)]
    pub fn assign_tokio_child(child: &tokio::process::Child) -> Option<Self> {
        child.raw_handle().and_then(Self::assign_raw)
    }

    /// Terminate every process in the job now (`TerminateJobObject`), then
    /// close it. On non-Windows targets this is a no-op.
    pub fn kill(self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;

            // SAFETY: the handle was created by CreateJobObjectW in `assign`
            // and is owned here exclusively. Terminating with exit code 1
            // ends every process in the job; dropping self then closes the
            // handle exactly once.
            unsafe {
                TerminateJobObject(self.handle.0, 1);
            }
        }
        #[cfg(not(windows))]
        drop(self);
    }
}

#[cfg(windows)]
impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: the handle was created by CreateJobObjectW in `assign` and
        // is owned here exclusively; closing the last handle terminates the
        // job (KILL_ON_JOB_CLOSE was set at assign) — the whole process tree.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle.0);
        }
    }
}

/// A `Command` for a helper process that must OUTLIVE its caller and every
/// command tree the caller starts (the shared `sccache` server, plan
/// d3336a8a step 7): no console window on Windows, its own process group on
/// Unix, and never assigned to a kill-on-close job — surviving the spawn that
/// started it is the whole point.
pub fn detached_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    platform_detach(&mut command);
    command
}

/// The platform half of [`detached_command`]: Win32 creation flags on Windows,
/// a fresh process group on Unix.
#[cfg(windows)]
fn platform_detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
}

/// The platform half of [`detached_command`]: a fresh process group, so no
/// group signal aimed at a command tree reaches the helper.
#[cfg(not(windows))]
fn platform_detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::KillOnCloseJob;

    /// Always reap the root and end anything a failed assertion left behind.
    /// If job assignment itself regressed, the job cannot clean up the tree,
    /// so use the system tree killer as the test-only fallback.
    struct TreeCleanup {
        child: Child,
        job: Option<KillOnCloseJob>,
    }

    impl Drop for TreeCleanup {
        fn drop(&mut self) {
            let had_job = self.job.is_some();
            drop(self.job.take());
            if !had_job {
                let _ = Command::new("taskkill")
                    .args(["/PID", &self.child.id().to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn pid_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: fixed query flags, no handle inheritance; the handle is
        // closed on every path below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: the handle comes from OpenProcess above; query access
        // suffices for GetExitCodeProcess.
        let mut exit_code: u32 = 0;
        let probed = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        // SAFETY: the handle is no longer used.
        unsafe { CloseHandle(handle) };
        probed != 0 && exit_code == STILL_ACTIVE as u32
    }

    fn assert_dead_within(pid: u32, description: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !pid_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !pid_alive(pid),
            "{description} (pid {pid}) survived the job"
        );
    }

    fn child_pid(parent: u32) -> u32 {
        let script = format!(
            "$end = [DateTime]::UtcNow.AddSeconds(5); do {{ \
             $p = Get-CimInstance Win32_Process -Filter 'ParentProcessId = {parent}' \
             | Select-Object -First 1; if ($p) {{ \
             [Console]::Out.Write($p.ProcessId); exit 0 }}; \
             Start-Sleep -Milliseconds 100 }} while ([DateTime]::UtcNow -lt $end); exit 1"
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .expect("query the spawned grandchild");
        assert!(
            output.status.success(),
            "grandchild query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("grandchild pid is UTF-8")
            .trim()
            .parse()
            .expect("grandchild query returned a pid")
    }

    #[test]
    fn kill_on_close_job_ends_the_whole_tree() {
        let child = Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd with a ping grandchild");
        let parent_pid = child.id();
        let job = KillOnCloseJob::assign_raw(child.as_raw_handle());
        let mut cleanup = TreeCleanup { child, job };
        assert!(
            cleanup.job.is_some(),
            "the freshly spawned cmd must be assignable to a job"
        );
        let grandchild_pid = child_pid(parent_pid);
        assert!(pid_alive(parent_pid), "the cmd parent must start alive");
        assert!(
            pid_alive(grandchild_pid),
            "the ping grandchild must start alive"
        );

        drop(cleanup.job.take());

        assert_dead_within(parent_pid, "cmd parent");
        assert_dead_within(grandchild_pid, "ping grandchild");
    }

    #[test]
    fn assign_failure_returns_none() {
        assert!(KillOnCloseJob::assign_raw(std::ptr::null_mut()).is_none());
    }
}
