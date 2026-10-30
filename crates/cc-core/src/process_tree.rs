//! Containing a child process and everything it starts, so a timeout can
//! kill all of it: cc-hook's chained status line command and cc-local's
//! `claude` CLI runs.
//!
//! Killing only the child is not enough: a `.cmd` shim or a shell runs the
//! real program as its own child, which would keep running (and keep the
//! inherited pipes open) after its parent is gone.
//!
//! - Windows: the child is put in a job object created with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Processes it starts join the
//!   job. [`ProcessTree::kill`] terminates the job; closing it
//!   ([`ProcessTree::close`], or dropping the [`ProcessTree`]) kills
//!   whatever is still in it, unless
//!   [`ProcessTree::leave`] ended the containment first. The child is
//!   assigned right after it is spawned, so a process it starts in that
//!   instant could stay outside the job.
//! - Unix: [`own_group`] makes the child the leader of a new process group,
//!   and [`ProcessTree::kill`] sends `SIGKILL` to the whole group. Closing
//!   or dropping the [`ProcessTree`] leaves the group alone.
//!
//! Setting up the job can fail (on very old Windows, or with a parent job
//! that forbids it); the tree is then empty and only the child itself can be
//! killed, which callers do anyway.

use std::process::{Child, Command};

/// Prepares `cmd` before it is spawned: on Unix the child leads a new
/// process group; nothing on Windows.
pub fn own_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// The process tree of one child; see the module docs.
#[cfg(windows)]
pub struct ProcessTree(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: the job handle is owned by this value and only used through
// thread-safe Win32 calls.
#[cfg(windows)]
unsafe impl Send for ProcessTree {}

#[cfg(windows)]
impl ProcessTree {
    /// Contains `child` (spawned after [`own_group`]) and what it starts.
    pub fn adopt(child: &Child) -> Self {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        // SAFETY: plain Win32 calls on a handle we own and on the child's
        // process handle, which `child` keeps open.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Self(std::ptr::null_mut());
            }
            let ok = set_kill_on_close(job, true)
                && AssignProcessToJobObject(job, child.as_raw_handle() as _) != 0;
            if !ok {
                CloseHandle(job);
                return Self(std::ptr::null_mut());
            }
            Self(job)
        }
    }

    /// Kills the child and every process it started that is still running.
    pub fn kill(&self) {
        if !self.0.is_null() {
            // SAFETY: a job handle we own.
            unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1) };
        }
    }

    /// Ends the containment the platform's way: closing the job kills
    /// whatever is still in it.
    pub fn close(self) {}

    /// Ends the containment and leaves whatever still runs alone.
    pub fn leave(self) {
        if !self.0.is_null() {
            // SAFETY: a job handle we own; it is closed right after, by drop.
            unsafe { set_kill_on_close(self.0, false) };
        }
    }
}

/// Sets or clears `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` on `job`.
///
/// # Safety
///
/// `job` must be a valid job object handle.
#[cfg(windows)]
unsafe fn set_kill_on_close(job: windows_sys::Win32::Foundation::HANDLE, on: bool) -> bool {
    use windows_sys::Win32::System::JobObjects::{
        JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    // SAFETY: `info` is a zeroed POD struct of the size passed.
    unsafe {
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        if on {
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: closed once; kill-on-close (when still set) ends what
            // is left in the job.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}

/// The process tree of one child; see the module docs.
#[cfg(unix)]
pub struct ProcessTree(i32);

#[cfg(unix)]
impl ProcessTree {
    /// Contains `child` (spawned after [`own_group`]) and what it starts.
    pub fn adopt(child: &Child) -> Self {
        Self(i32::try_from(child.id()).unwrap_or(0))
    }

    /// Kills the child and every process it started that is still running
    /// in its process group.
    pub fn kill(&self) {
        if self.0 > 0 {
            // SAFETY: kill(2) on our child's process group.
            unsafe { libc::kill(-self.0, libc::SIGKILL) };
        }
    }

    /// Ends the containment the platform's way: the process group is left
    /// alone.
    pub fn close(self) {}

    /// Ends the containment and leaves whatever still runs alone.
    pub fn leave(self) {}
}

/// The process tree of one child; nothing is contained on this platform.
#[cfg(not(any(windows, unix)))]
pub struct ProcessTree;

#[cfg(not(any(windows, unix)))]
impl ProcessTree {
    /// Nothing to contain on this platform.
    pub fn adopt(_child: &Child) -> Self {
        Self
    }

    /// Nothing to kill beyond the child itself on this platform.
    pub fn kill(&self) {}

    /// Ends the containment.
    pub fn close(self) {}

    /// Ends the containment.
    pub fn leave(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A shell that starts a long-running grandchild writing its pid to
    /// `pid_file`, then waits for it.
    fn shell_with_grandchild(pid_file: &std::path::Path) -> Command {
        if cfg!(windows) {
            let script = format!(
                "$p = Start-Process -PassThru -WindowStyle Hidden ping -ArgumentList '-n','60','127.0.0.1'; \
                 Set-Content -LiteralPath '{}' $p.Id; Wait-Process -Id $p.Id",
                pid_file.display()
            );
            let mut cmd = Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
            cmd
        } else {
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c").arg(format!(
                "sleep 60 & echo $! > '{}'; wait",
                pid_file.display()
            ));
            cmd
        }
    }

    #[cfg(windows)]
    fn running(pid: u32) -> bool {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
    }

    #[cfg(unix)]
    fn running(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        i32::try_from(pid).is_ok_and(|p| unsafe { libc::kill(p, 0) } == 0)
    }

    fn grandchild_pid(pid_file: &std::path::Path) -> u32 {
        let started = Instant::now();
        loop {
            if let Some(pid) = std::fs::read_to_string(pid_file)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                return pid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the grandchild never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_gone(pid: u32) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if !running(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn kill_ends_the_grandchildren_too() {
        let dir = std::env::temp_dir().join(format!("cc-core-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("grandchild.pid");
        let _ = std::fs::remove_file(&pid_file);
        let mut cmd = shell_with_grandchild(&pid_file);
        own_group(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let tree = ProcessTree::adopt(&child);
        let grandchild = grandchild_pid(&pid_file);
        assert!(running(grandchild));

        tree.kill();
        let _ = child.kill();
        let _ = child.wait();
        let gone = wait_gone(grandchild);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(gone, "grandchild {grandchild} survived the kill");
    }

    #[test]
    fn leave_keeps_what_still_runs() {
        let dir = std::env::temp_dir().join(format!("cc-core-tree-leave-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("grandchild.pid");
        let _ = std::fs::remove_file(&pid_file);
        let mut cmd = shell_with_grandchild(&pid_file);
        own_group(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let tree = ProcessTree::adopt(&child);
        let grandchild = grandchild_pid(&pid_file);

        // Only the child is killed; leaving (and on Windows closing the job)
        // keeps the grandchild.
        let _ = child.kill();
        let _ = child.wait();
        tree.leave();
        std::thread::sleep(Duration::from_millis(300));
        let survived = running(grandchild);
        // Clean up the grandchild by its pid.
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &grandchild.to_string()])
            .output();
        #[cfg(unix)]
        // SAFETY: kill(2) on the grandchild this test started.
        unsafe {
            libc::kill(grandchild as i32, libc::SIGKILL)
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert!(survived, "grandchild {grandchild} died with the tree");
    }
}
