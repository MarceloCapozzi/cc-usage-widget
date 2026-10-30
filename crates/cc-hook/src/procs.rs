//! Finds the Claude Code process that ran this hook.
//!
//! Claude Code starts hooks through a shell, so the chain is typically
//! `claude.exe -> bash.exe|cmd.exe -> cc-hook.exe`. The receiver walks up at
//! most [`MAX_PARENT_LEVELS`] parents and takes the first whose executable
//! name starts with `claude`. On Windows each ancestor is queried directly
//! (`OpenProcess` + `NtQueryInformationProcess`), falling back to a
//! Toolhelp32 snapshot; on Linux it reads `/proc/<pid>/stat`. Anything that goes
//! wrong yields `None`: the pid is a nice-to-have, never a reason to fail.

/// How far up the parent chain the search goes.
pub const MAX_PARENT_LEVELS: usize = 5;

/// One process as the walk sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    /// Parent process id (0 when unknown or at the root).
    pub parent: u32,
    /// Executable name, e.g. `claude.exe` (Windows) or the `comm` (Linux).
    pub name: String,
}

/// Whether an executable name belongs to Claude Code (`claude.exe`, `claude`).
pub fn is_claude_exe(name: &str) -> bool {
    name.trim().to_ascii_lowercase().starts_with("claude")
}

/// Walks up from `start` (excluded) through at most [`MAX_PARENT_LEVELS`]
/// ancestors, using `lookup` to resolve each pid, and returns the first whose
/// name satisfies [`is_claude_exe`].
pub fn walk_parents(start: u32, lookup: impl Fn(u32) -> Option<ProcInfo>) -> Option<u32> {
    let mut pid = lookup(start)?.parent;
    for _ in 0..MAX_PARENT_LEVELS {
        if pid == 0 {
            return None;
        }
        let info = lookup(pid)?;
        if is_claude_exe(&info.name) {
            return Some(pid);
        }
        if info.parent == pid {
            return None;
        }
        pid = info.parent;
    }
    None
}

/// The pid of the Claude Code process above this one, if any.
pub fn find_claude_pid() -> Option<u32> {
    imp::find_claude_pid()
}

#[cfg(windows)]
mod imp {
    use super::{walk_parents, ProcInfo};
    use std::cell::Cell;
    use std::collections::HashMap;
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_BASIC_INFORMATION, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// Fast path first: query each ancestor directly (well under a
    /// millisecond). A Toolhelp32 snapshot of every process costs ~10 ms on a
    /// busy machine, so it is only the fallback for when an ancestor cannot be
    /// opened (e.g. an elevated process).
    pub fn find_claude_pid() -> Option<u32> {
        let blocked = Cell::new(false);
        let fast = walk_parents(std::process::id(), |pid| {
            let info = query(pid);
            if info.is_none() {
                blocked.set(true);
            }
            info
        });
        if fast.is_some() || !blocked.get() {
            return fast;
        }
        let table = snapshot()?;
        walk_parents(std::process::id(), |pid| table.get(&pid).cloned())
    }

    /// Parent pid and executable file name of one process.
    fn query(pid: u32) -> Option<ProcInfo> {
        // SAFETY: plain Win32/NT calls on a handle checked for null and closed
        // on every path; output buffers are correctly sized locals.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut basic: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
            let mut returned = 0u32;
            let status = NtQueryInformationProcess(
                handle,
                ProcessBasicInformation,
                (&mut basic as *mut PROCESS_BASIC_INFORMATION).cast(),
                std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                &mut returned,
            );
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let named =
                QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(handle);
            if status < 0 || named == 0 {
                return None;
            }
            let path = String::from_utf16_lossy(buf.get(..len as usize)?);
            let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
            Some(ProcInfo {
                parent: u32::try_from(basic.InheritedFromUniqueProcessId).unwrap_or(0),
                name,
            })
        }
    }

    /// pid -> (parent, exe name) for every process, from one Toolhelp32 snapshot.
    fn snapshot() -> Option<HashMap<u32, ProcInfo>> {
        // SAFETY: plain Win32 calls. The snapshot handle is checked and closed
        // on every path; PROCESSENTRY32W is a POD struct initialised with its
        // size as the API requires.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE || snap.is_null() {
                return None;
            }
            let mut table = HashMap::with_capacity(512);
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry);
            while ok != 0 {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                table.insert(
                    entry.th32ProcessID,
                    ProcInfo {
                        parent: entry.th32ParentProcessID,
                        name,
                    },
                );
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
            Some(table)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn direct_query_agrees_with_the_snapshot() {
            let me = std::process::id();
            let direct = query(me).expect("own process can be queried");
            let table = snapshot().expect("snapshot works");
            assert_eq!(table.get(&me), Some(&direct));
            assert!(direct.name.to_ascii_lowercase().ends_with(".exe"));
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{walk_parents, ProcInfo};

    pub fn find_claude_pid() -> Option<u32> {
        walk_parents(std::process::id(), read_stat)
    }

    /// Parses `/proc/<pid>/stat`: `pid (comm) state ppid ...`. The comm may
    /// contain spaces and parentheses, so split at the last `)`.
    fn read_stat(pid: u32) -> Option<ProcInfo> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        super::parse_stat(&text)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn find_claude_pid() -> Option<u32> {
        None
    }
}

/// Parses the content of a Linux `/proc/<pid>/stat` file into a [`ProcInfo`].
pub fn parse_stat(text: &str) -> Option<ProcInfo> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_string();
    let mut rest = text.get(close + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let parent = rest.next()?.parse().ok()?;
    Some(ProcInfo { parent, name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn table(rows: &[(u32, u32, &str)]) -> HashMap<u32, ProcInfo> {
        rows.iter()
            .map(|&(pid, parent, name)| {
                (
                    pid,
                    ProcInfo {
                        parent,
                        name: name.into(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn finds_claude_through_a_shell() {
        let t = table(&[
            (10, 9, "cc-hook.exe"),
            (9, 8, "bash.exe"),
            (8, 7, "claude.exe"),
            (7, 1, "Code.exe"),
        ]);
        assert_eq!(walk_parents(10, |p| t.get(&p).cloned()), Some(8));
    }

    #[test]
    fn matches_linux_and_mixed_case_names() {
        assert!(is_claude_exe("claude"));
        assert!(is_claude_exe("Claude.exe"));
        assert!(!is_claude_exe("node.exe"));
        assert!(!is_claude_exe("cc-hook"));
    }

    #[test]
    fn gives_up_after_five_levels_or_on_gaps_and_cycles() {
        let t = table(&[
            (10, 9, "cc-hook"),
            (9, 8, "a"),
            (8, 7, "b"),
            (7, 6, "c"),
            (6, 5, "d"),
            (5, 4, "e"),
            (4, 1, "claude"),
        ]);
        assert_eq!(walk_parents(10, |p| t.get(&p).cloned()), None);
        let t = table(&[(10, 9, "cc-hook"), (9, 9, "self-parent")]);
        assert_eq!(walk_parents(10, |p| t.get(&p).cloned()), None);
        let t = table(&[(10, 99, "cc-hook")]);
        assert_eq!(walk_parents(10, |p| t.get(&p).cloned()), None);
        assert_eq!(walk_parents(10, |_| None), None);
    }

    #[test]
    fn parses_proc_stat_lines() {
        let s = "1234 (claude) S 1200 1234 1200 0 -1 4194560 ...";
        assert_eq!(
            parse_stat(s),
            Some(ProcInfo {
                parent: 1200,
                name: "claude".into()
            })
        );
        let s = "77 (weird ) name)) R 5 77";
        assert_eq!(
            parse_stat(s),
            Some(ProcInfo {
                parent: 5,
                name: "weird ) name)".into()
            })
        );
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn real_lookup_does_not_fail() {
        // Under cargo test there is usually no claude ancestor; either way it must not panic.
        let _ = find_claude_pid();
    }
}
