//! The Win32 side: process snapshot, window enumeration, console probe and
//! window activation. Decisions live in [`crate::plan`]; this module only
//! gathers facts and performs the final activation.

use std::sync::Mutex;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, GetConsoleWindow, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_MENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, EnumWindows, FlashWindowEx, GetClassNameW,
    GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, SetForegroundWindow, ShowWindow, FLASHWINFO, FLASHW_ALL,
    FLASHW_TIMERNOFG, GWL_EXSTYLE, GW_OWNER, SW_RESTORE, WS_EX_TOOLWINDOW,
};

use crate::plan::{self, ConsoleProbe, Link, Proc, ProcTable};
use crate::{
    FocusOutcome, FocusReport, FocusRequest, HostKind, Technique, WindowInfo, ENTRYPOINT_VSCODE,
};

/// Attaching to a console is process-wide state, so probes never overlap.
static CONSOLE_LOCK: Mutex<()> = Mutex::new(());

/// Locates the window for `request` and, when `activate`, brings it to the front.
pub(crate) fn run(request: &FocusRequest<'_>, activate: bool) -> FocusReport {
    let procs = process_table();
    let chain = chain(request.pid, &procs);
    if chain.is_empty() {
        return FocusReport::bare(FocusOutcome::NotFound);
    }
    let windows = top_level_windows();
    let console = if request.entrypoint == Some(ENTRYPOINT_VSCODE) {
        None
    } else {
        probe_console(&console_candidates(&chain))
    };
    let Some(located) = plan::locate(&chain, &procs, &windows, console.as_ref(), request) else {
        return FocusReport::bare(FocusOutcome::NotFound);
    };
    let technique = activate.then(|| activate_window(located.window.hwnd, located.window.pid));
    FocusReport {
        outcome: located.outcome,
        route: Some(located.route),
        window_exe: procs.get(&located.window.pid).map(|p| p.exe.clone()),
        window: Some(located.window),
        technique,
    }
}

/// Every running process, from one Toolhelp32 snapshot.
pub(crate) fn process_table() -> ProcTable {
    let mut procs = ProcTable::new();
    // SAFETY: the snapshot handle is checked and closed; the entry is a plain
    // struct whose size field is set before use, as Toolhelp32 requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return procs;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snapshot, &mut entry);
        while ok != 0 {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            procs.insert(
                entry.th32ProcessID,
                Proc {
                    ppid: entry.th32ParentProcessID,
                    exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
                },
            );
            ok = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
    }
    procs
}

/// The parent chain of `pid`, cut where a parent id was reused by a process
/// created after its supposed child.
pub(crate) fn chain(pid: u32, procs: &ProcTable) -> Vec<Link> {
    plan::walk_chain(pid, procs, |child, parent| {
        match (creation_time(child), creation_time(parent)) {
            (Some(child), Some(parent)) => parent > child,
            _ => false,
        }
    })
}

/// Process creation time in 100 ns units, when the process can be opened.
fn creation_time(pid: u32) -> Option<u64> {
    // SAFETY: the handle is checked and closed; the FILETIME out-params are locals.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut times: [FILETIME; 4] = std::mem::zeroed();
        let [created, exited, kernel, user] = &mut times;
        let ok = GetProcessTimes(handle, created, exited, kernel, user);
        CloseHandle(handle);
        (ok != 0)
            .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

/// Visible, unowned, titled, non-tool top-level windows, in Z order.
pub(crate) fn top_level_windows() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::new();
    // SAFETY: the callback only runs during EnumWindows, while `out` is alive
    // and exclusively borrowed through the LPARAM.
    unsafe {
        EnumWindows(
            Some(collect_window),
            &mut out as *mut Vec<WindowInfo> as LPARAM,
        );
    }
    out
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: `lparam` is the `&mut Vec` passed by `top_level_windows`.
    let out = unsafe { &mut *(lparam as *mut Vec<WindowInfo>) };
    if let Some(window) = candidate(hwnd) {
        out.push(window);
    }
    1
}

/// The window as an activation candidate, or `None` when it is not one.
fn candidate(hwnd: HWND) -> Option<WindowInfo> {
    // SAFETY: plain queries on a window handle; buffers are sized locals.
    unsafe {
        if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
            return None;
        }
        if (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW != 0 {
            return None;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if len <= 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        let mut class = [0u16; 256];
        let class_len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32).max(0);
        let class = String::from_utf16_lossy(&class[..class_len as usize]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        Some(WindowInfo {
            hwnd: hwnd as isize,
            pid,
            title,
            class,
            minimized: IsIconic(hwnd) != 0,
        })
    }
}

/// Processes worth attaching to: the session itself, then the shells above it.
fn console_candidates(chain: &[Link]) -> Vec<u32> {
    chain
        .iter()
        .enumerate()
        .take_while(|(_, (_, _, exe))| HostKind::from_exe(exe) != HostKind::Boundary)
        .filter(|(i, (_, _, exe))| *i == 0 || HostKind::from_exe(exe) == HostKind::Shell)
        .map(|(_, (pid, _, _))| *pid)
        .collect()
}

/// Attaches to the first console among `pids` for a moment to read its window.
///
/// Skipped when this process has a console of its own: attaching to another
/// one would require giving ours up.
fn probe_console(pids: &[u32]) -> Option<ConsoleProbe> {
    let _guard = CONSOLE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: console attachment is serialised by the lock above and undone
    // before returning; Ctrl+C is ignored while attached so a key press in
    // the session's console cannot terminate this process.
    unsafe {
        if !GetConsoleWindow().is_null() {
            return None;
        }
        for &pid in pids {
            SetConsoleCtrlHandler(None, 1);
            if AttachConsole(pid) == 0 {
                SetConsoleCtrlHandler(None, 0);
                continue;
            }
            let hwnd = GetConsoleWindow();
            FreeConsole();
            SetConsoleCtrlHandler(None, 0);
            if hwnd.is_null() {
                continue;
            }
            let owner = GetWindow(hwnd, GW_OWNER);
            return Some(ConsoleProbe {
                hwnd: hwnd as isize,
                owner: (!owner.is_null()).then_some(owner as isize),
            });
        }
    }
    None
}

/// Restores and brings `hwnd` to the front, trying each workaround in turn.
fn activate_window(hwnd: isize, pid: u32) -> Technique {
    let hwnd = hwnd as HWND;
    // SAFETY: Win32 calls on a window handle that was valid moments ago; a
    // stale handle makes them fail harmlessly. Thread input is detached again
    // right after use.
    unsafe {
        if IsWindow(hwnd) == 0 {
            return Technique::Failed;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        if GetForegroundWindow() == hwnd {
            return Technique::AlreadyForeground;
        }
        // Lets the window's process keep the foreground if it reacts by activating itself.
        AllowSetForegroundWindow(pid);
        if SetForegroundWindow(hwnd) != 0 && became_foreground(hwnd) {
            return Technique::Direct;
        }

        let foreground = GetForegroundWindow();
        let foreground_thread = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
        let me = GetCurrentThreadId();
        if foreground_thread != 0
            && foreground_thread != me
            && AttachThreadInput(me, foreground_thread, 1) != 0
        {
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
            AttachThreadInput(me, foreground_thread, 0);
            if became_foreground(hwnd) {
                return Technique::AttachThreadInput;
            }
        }

        // A synthetic key press makes this process the last input source, which
        // grants foreground rights. Down and up are sent together, before the
        // switch, so the target never sees a lone ALT (VS Code would focus its menu bar).
        tap_alt();
        SetForegroundWindow(hwnd);
        if became_foreground(hwnd) {
            return Technique::AltKey;
        }

        let flash = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        FlashWindowEx(&flash);
    }
    Technique::Failed
}

/// Polls briefly: the foreground change is not always visible immediately.
fn became_foreground(hwnd: HWND) -> bool {
    for _ in 0..5 {
        // SAFETY: a parameterless query.
        if unsafe { GetForegroundWindow() } == hwnd {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn tap_alt() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [key(0), key(KEYEVENTF_KEYUP)];
    // SAFETY: a fully initialised local array with its exact length and element size.
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_table_contains_this_process() {
        let procs = process_table();
        let me = procs
            .get(&std::process::id())
            .expect("current process in snapshot");
        assert!(me.exe.to_ascii_lowercase().ends_with(".exe"));
    }

    #[test]
    fn this_process_was_created_after_its_parent() {
        let procs = process_table();
        let me = std::process::id();
        let mine = creation_time(me).expect("own creation time");
        let parent = procs[&me].ppid;
        if let Some(theirs) = creation_time(parent) {
            assert!(theirs <= mine);
        }
    }

    #[test]
    fn console_candidates_are_the_session_and_shells() {
        let chain: Vec<Link> = vec![
            (10, 8, "claude.exe".into()),
            (8, 6, "pwsh.exe".into()),
            (6, 5, "node.exe".into()),
            (5, 4, "cmd.exe".into()),
            (4, 3, "explorer.exe".into()),
            (3, 0, "cmd.exe".into()),
        ];
        assert_eq!(console_candidates(&chain), vec![10, 8, 5]);
    }

    #[test]
    fn enumerated_windows_are_unowned_visible_candidates() {
        for w in top_level_windows() {
            let hwnd = w.hwnd as HWND;
            // SAFETY: plain queries; a window closed since enumeration just fails.
            unsafe {
                if IsWindow(hwnd) != 0 {
                    assert!(GetWindow(hwnd, GW_OWNER).is_null());
                }
            }
        }
    }
}
