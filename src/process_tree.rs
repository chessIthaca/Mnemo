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

/// The Win32 creation flags of a detached child: a hidden console
/// (`CREATE_NO_WINDOW`) in its own process group. `DETACHED_PROCESS` is
/// deliberately absent — see [`detach_command`].
#[cfg(windows)]
pub const DETACHED_CREATION_FLAGS: u32 = 0x0800_0000 | 0x0000_0200;

/// The flag `DETACHED_PROCESS`, named only for the assertion that it is NOT
/// part of [`DETACHED_CREATION_FLAGS`] (test-visible documentation of the
/// console-window fix: a console-less child makes every console-subsystem
/// grandchild pop a NEW console window).
#[cfg(windows)]
pub const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Detach a child from this process: on Windows `CREATE_NO_WINDOW |
/// CREATE_NEW_PROCESS_GROUP` (a HIDDEN console the child and every
/// console-subsystem grandchild inherit, and a `CTRL_C` for this process's
/// group never reaches it), on Unix its own process group. The caller must NOT
/// assign the child to a kill-on-close job — surviving the spawn that started
/// it is the whole point.
///
/// Deliberately NOT `DETACHED_PROCESS`: a detached process has NO console, so
/// every console-subsystem grandchild it starts makes the OS allocate a NEW
/// one — on Windows that is a popping Terminal window per grandchild (the
/// user saw a swarm of them). `CREATE_NO_WINDOW` gives the child a hidden
/// console instead, which its grandchildren inherit invisibly.
///
/// THE one place the detached spawn flags are defined: the shared services
/// ([`crate::host::shared_service::detach`]) and the detached helpers
/// ([`detached_command`]) both come through here.
pub fn detach_command(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(DETACHED_CREATION_FLAGS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(not(any(windows, unix)))]
    let _ = command;
}

/// A `Command` for a helper process that must OUTLIVE its caller and every
/// command tree the caller starts (the shared `sccache` server, plan
/// d3336a8a step 7): no console window on Windows, its own process group on
/// Unix, and never assigned to a kill-on-close job — surviving the spawn that
/// started it is the whole point.
pub fn detached_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    detach_command(&mut command);
    command
}

/// Is the process `pid` still running?
///
/// On Windows a hard-stopped process can leave its handle open somewhere, so
/// the pid counts as live only when `GetExitCodeProcess` yields `STILL_ACTIVE`
/// (a process that really exits with code 259 is indistinguishable — accepted:
/// every caller uses this as a hint, never as a lock). On Unix it is
/// `ps -o stat=`: the pid is alive only when ps prints a state for it, and a
/// ZOMBIE — a killed child its parent has not reaped yet — counts as dead,
/// although a bare `ps -p` would still list it (the v2.7.0 macOS build: a
/// stub the watchdog had killed kept passing its own liveness poll).
///
/// The one implementation every caller shares (the app's instance markers, the
/// shared-service leases and their watchdog), so liveness can never differ
/// between the components that must agree on it.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: fixed flags, no handle inheritance; the handle is
        // closed on every path below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: the handle comes from OpenProcess above and outlives
        // this call; query access suffices for GetExitCodeProcess.
        let mut exit_code: u32 = 0;
        let probed = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        // SAFETY: the handle is no longer used.
        unsafe { CloseHandle(handle) };
        probed != 0 && exit_code == STILL_ACTIVE as u32
    }
    #[cfg(not(windows))]
    {
        // `stat=` prints the process state instead of a bare yes: a ZOMBIE —
        // a killed child its parent has not reaped yet — still makes a plain
        // `ps -p` exit zero, so a server an instance spawned and never
        // reaps would stay "alive" to its own liveness poll after a crash
        // (the v2.7.0 macOS build). Alive only when ps succeeds and prints
        // a state that is not a zombie's (`Z…`).
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "stat="])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .is_some_and(|state| !state.is_empty() && !state.starts_with('Z'))
    }
}

/// A string identifying WHEN the process `pid` was created — the pid-reuse
/// guard for every "kill pid" decision a shared service makes.
///
/// Pids are recycled: a server dies, another process inherits its pid, and a
/// stale lease or descriptor would name an innocent process. The creation
/// identity is captured at spawn/publish time and compared again before any
/// kill, so a reused pid never gets signalled. It is only ever COMPARED for
/// equality — never parsed — so any stable per-process value works:
///
/// - **Windows**: the process creation `FILETIME` (`GetProcessTimes`, 100 ns
///   intervals since 1601) as a decimal string — unique per process.
/// - **Unix**: `ps -p <pid> -o lstart=` — the start time as text. Second
///   granularity, so a pid would have to be reused within the same second to
///   fool it; deliberate reuse that fast is not a real scenario.
///
/// `None` when the identity cannot be determined (a dead pid, an unopenable
/// process, a `ps`-less Unix). Callers treat `None` as "unproven" and skip
/// the kill, never as "safe".
pub fn process_creation_id(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
        use windows_sys::Win32::System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: fixed flags, no handle inheritance; the handle is
        // closed on every path below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: the handle comes from OpenProcess above and outlives this
        // call; the four FILETIMEs are valid out-parameters.
        let probed = unsafe {
            GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
        };
        // SAFETY: the handle is no longer used.
        unsafe { CloseHandle(handle) };
        (probed != 0).then(|| {
            ((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
                .to_string()
        })
    }
    #[cfg(not(windows))]
    {
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .ok()?;
        (output.status.success() && !output.stdout.is_empty()).then(|| {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        })
    }
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

/// Unix-only liveness tests: the killed-but-unreaped child — a ZOMBIE that
/// a bare `ps -p` still lists.
#[cfg(all(test, unix))]
mod unix_tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::pid_alive;

    /// Regression (the v2.7.0 macOS release build): a child that has exited
    /// but that its parent has not reaped yet is a ZOMBIE, and `ps -p` still
    /// lists it — so the shared services' liveness polls reported a killed
    /// server (the watchdog test's stub, a crashed shared laya-serve) as
    /// alive forever. pid_alive must treat a zombie as dead.
    #[test]
    fn pid_alive_treats_a_zombie_as_dead() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the short-lived child");
        let pid = child.id();
        // Wait until it has exited WITHOUT reaping it: an exited child its
        // parent never reaps is a zombie, and ps reports its state as `Z…`.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .expect("read the child's state");
            let state = String::from_utf8_lossy(&state.stdout).trim().to_string();
            if state.starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the short-lived child never became a zombie: state {state:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !pid_alive(pid),
            "a zombie (a killed child this parent never reaped) must count as dead"
        );
        // Reap so the test leaves no zombie behind.
        let _ = child.wait();
    }
}
