//! Whether a process still runs, and when it started.
//!
//! [`pid_alive`] tells a live process from one that has exited, and
//! [`start_time`] gives an opaque value that only matches for the same
//! process: a pid that was reused by another process has another start time.
//! Both answer `None` when the OS cannot tell (pid 0, no permission, or a
//! platform other than Windows and Linux); callers then fall back to whatever
//! they did before.
//!
//! - Windows: `OpenProcess` + `GetExitCodeProcess`, and the creation time
//!   from `GetProcessTimes` (100 ns ticks since 1601).
//! - Linux: `/proc/<pid>/stat`: a missing entry, or a zombie, is an exited
//!   process; field 22 (`starttime`, clock ticks since boot) is the start time.

/// Whether the process `pid` still runs. `Some(false)` only when the OS says
/// it has exited or never existed; `None` when that cannot be told.
pub fn pid_alive(pid: u32) -> Option<bool> {
    if pid == 0 {
        return None;
    }
    imp::alive(pid)
}

/// When the process `pid` started, as an opaque value: equal for the same
/// process, different for another process that later got the same pid.
/// `None` when it cannot be read.
pub fn start_time(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    imp::start_time(pid)
}

/// Whether `pid` is still the process that had `started` as its
/// [`start_time`]: `Some(false)` when it has exited or the pid now belongs to
/// another process, `Some(true)` when both the pid and its start time match,
/// `None` when the OS cannot tell (or `started` is unknown and the pid is
/// alive).
pub fn same_process(pid: u32, started: Option<u64>) -> Option<bool> {
    match pid_alive(pid)? {
        false => Some(false),
        true => match (started, start_time(pid)) {
            (Some(then), Some(now)) => Some(then == now),
            _ => None,
        },
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, FILETIME, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub fn alive(pid: u32) -> Option<bool> {
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // Access denied and the like mean the process exists.
                return Some(GetLastError() != ERROR_INVALID_PARAMETER);
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            if ok == 0 {
                return None;
            }
            Some(code == STILL_ACTIVE as u32)
        }
    }

    pub fn start_time(pid: u32) -> Option<u64> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: plain Win32 calls on valid out-pointers; the handle is
        // closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
            CloseHandle(handle);
            if ok == 0 {
                return None;
            }
        }
        Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    /// The fields of `/proc/<pid>/stat` after the command name, which is in
    /// parentheses and may itself contain spaces and parentheses.
    fn stat_fields(pid: u32) -> std::io::Result<Vec<String>> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let after = text
            .rfind(')')
            .map(|i| &text[i + 1..])
            .ok_or_else(|| std::io::Error::other("unexpected /proc stat"))?;
        Ok(after.split_whitespace().map(str::to_string).collect())
    }

    pub fn alive(pid: u32) -> Option<bool> {
        match stat_fields(pid) {
            // Field 3, the state: a zombie (Z) or dead (X) process has exited.
            Ok(fields) => Some(!matches!(
                fields.first().map(String::as_str),
                Some("Z" | "X")
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    }

    pub fn start_time(pid: u32) -> Option<u64> {
        // Field 22 overall; the list starts at field 3.
        stat_fields(pid).ok()?.get(22 - 3)?.parse().ok()
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn alive(_pid: u32) -> Option<bool> {
        None
    }

    pub fn start_time(_pid: u32) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exited_pid() -> u32 {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd.exe" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit 0"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn this_process_is_alive_and_an_exited_one_is_not() {
        assert_eq!(pid_alive(0), None);
        assert_eq!(start_time(0), None);
        if cfg!(any(windows, target_os = "linux")) {
            assert_eq!(pid_alive(std::process::id()), Some(true));
            assert_eq!(pid_alive(exited_pid()), Some(false), "exited (and reaped)");
        }
    }

    #[test]
    fn the_start_time_identifies_this_process() {
        let me = std::process::id();
        if !cfg!(any(windows, target_os = "linux")) {
            assert_eq!(same_process(me, None), None);
            return;
        }
        let started = start_time(me).expect("own start time");
        assert_eq!(start_time(me), Some(started), "stable");
        assert_eq!(same_process(me, Some(started)), Some(true));
        assert_eq!(
            same_process(me, Some(started.wrapping_add(1))),
            Some(false),
            "the pid now belongs to another process"
        );
        assert_eq!(same_process(me, None), None, "unknown start time");
        assert_eq!(same_process(exited_pid(), Some(started)), Some(false));
    }
}
