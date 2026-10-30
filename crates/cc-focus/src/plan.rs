//! Platform-independent decisions: walking the parent chain over a process
//! table and choosing which window to activate. The Windows module gathers
//! the inputs (snapshot, windows, console probe); everything here is pure and
//! unit-tested with synthetic data, so no test ever touches the real desktop.

use std::collections::HashMap;

use crate::{
    FocusOutcome, FocusRequest, HostKind, ProcessInfo, Route, WindowInfo, CONSOLE_WINDOW_CLASS,
    ENTRYPOINT_VSCODE, MAX_ANCESTORS,
};

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proc {
    pub ppid: u32,
    pub exe: String,
}

/// Every process by pid, from one snapshot.
pub(crate) type ProcTable = HashMap<u32, Proc>;

/// One link of a parent chain: pid, parent pid, exe.
pub(crate) type Link = (u32, u32, String);

/// What attaching to the session's console revealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConsoleProbe {
    /// The console window (`GetConsoleWindow`), visible or not.
    pub hwnd: isize,
    /// Its owner window (`GW_OWNER`), set by ConPTY hosts such as Windows Terminal.
    pub owner: Option<isize>,
}

/// The window chosen for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Located {
    pub route: Route,
    pub window: WindowInfo,
    pub outcome: FocusOutcome,
}

/// Walks from `pid` up through its parents, at most [`MAX_ANCESTORS`] above it.
///
/// Stops at a missing process, a cycle, pid 0, or when `is_stale(child, parent)`
/// says the recorded parent id now belongs to a newer, unrelated process.
pub(crate) fn walk_chain(
    pid: u32,
    procs: &ProcTable,
    mut is_stale: impl FnMut(u32, u32) -> bool,
) -> Vec<Link> {
    let mut chain: Vec<Link> = Vec::new();
    let mut current = pid;
    while chain.len() <= MAX_ANCESTORS {
        let Some(proc) = procs.get(&current) else {
            break;
        };
        chain.push((current, proc.ppid, proc.exe.clone()));
        let parent = proc.ppid;
        if parent == 0
            || parent == current
            || chain.iter().any(|(p, _, _)| *p == parent)
            || !procs.contains_key(&parent)
            || is_stale(current, parent)
        {
            break;
        }
        current = parent;
    }
    chain
}

/// Turns chain links into [`ProcessInfo`] rows, marking which own a window.
pub(crate) fn describe_chain(chain: &[Link], windows: &[WindowInfo]) -> Vec<ProcessInfo> {
    chain
        .iter()
        .map(|(pid, parent_pid, exe)| ProcessInfo {
            pid: *pid,
            parent_pid: *parent_pid,
            exe: exe.clone(),
            has_window: windows.iter().any(|w| w.pid == *pid),
        })
        .collect()
}

/// Chooses the window to activate for `request`, or `None` when there is none.
///
/// `windows` holds only activation candidates (visible, unowned, titled), in
/// Z order. See the crate documentation for the order of the routes.
pub(crate) fn locate(
    chain: &[Link],
    procs: &ProcTable,
    windows: &[WindowInfo],
    console: Option<&ConsoleProbe>,
    request: &FocusRequest<'_>,
) -> Option<Located> {
    let vscode = request.entrypoint == Some(ENTRYPOINT_VSCODE);
    let exe_of = |pid: u32| procs.get(&pid).map(|p| p.exe.as_str()).unwrap_or("");

    if let (false, Some(console)) = (vscode, console) {
        if let Some(window) = windows.iter().find(|w| w.hwnd == console.hwnd) {
            return Some(Located {
                route: Route::ConsoleWindow,
                window: window.clone(),
                outcome: FocusOutcome::Precise,
            });
        }
        let owner = console
            .owner
            .and_then(|o| windows.iter().find(|w| w.hwnd == o));
        if let Some(window) = owner {
            let outcome = window_outcome(window, exe_of(window.pid));
            if outcome.found() {
                return Some(Located {
                    route: Route::PseudoConsoleOwner,
                    window: window.clone(),
                    outcome,
                });
            }
        }
    }

    for (pid, _, exe) in chain {
        if HostKind::from_exe(exe) == HostKind::Boundary {
            break;
        }
        let own: Vec<&WindowInfo> = windows.iter().filter(|w| w.pid == *pid).collect();
        let (route, candidates) = if !own.is_empty() {
            (Route::Ancestor, own)
        } else if !vscode {
            let hosts: Vec<u32> = procs
                .iter()
                .filter(|(_, p)| {
                    p.ppid == *pid && HostKind::from_exe(&p.exe) == HostKind::ConsoleHost
                })
                .map(|(host_pid, _)| *host_pid)
                .collect();
            let hosted: Vec<&WindowInfo> = windows
                .iter()
                .filter(|w| hosts.contains(&w.pid) && w.class == CONSOLE_WINDOW_CLASS)
                .collect();
            (Route::ConsoleHostChild, hosted)
        } else {
            continue;
        };
        if let Some(window) = best_for_cwd(&candidates, request.cwd) {
            let outcome = window_outcome(window, exe_of(window.pid));
            if outcome.found() {
                return Some(Located {
                    route,
                    window: window.clone(),
                    outcome,
                });
            }
        }
    }
    None
}

/// Precise for classic console windows, otherwise what the owning host implies.
fn window_outcome(window: &WindowInfo, exe: &str) -> FocusOutcome {
    if window.class == CONSOLE_WINDOW_CLASS {
        FocusOutcome::Precise
    } else {
        HostKind::from_exe(exe).outcome()
    }
}

/// Among windows of one process, prefers the one whose title names the deepest
/// component of `cwd`; ties go to the topmost (first in Z order).
///
/// A title segment equal to the component (VS Code titles read
/// `file - folder - Visual Studio Code`) beats a mere substring, so `repo`
/// prefers `x - repo - Code` over `x - repo-next - Code`.
fn best_for_cwd<'w>(candidates: &[&'w WindowInfo], cwd: Option<&str>) -> Option<&'w WindowInfo> {
    let components: Vec<String> = cwd
        .unwrap_or("")
        .split(['\\', '/'])
        .filter(|c| c.len() >= 2 && !c.ends_with(':'))
        .map(|c| c.to_lowercase())
        .collect();
    let deepest: Vec<&str> = components
        .iter()
        .rev()
        .take(4)
        .map(String::as_str)
        .collect();
    let score = |w: &WindowInfo| title_score(&w.title, &deepest);
    let mut best: Option<(&WindowInfo, usize)> = None;
    for w in candidates {
        let s = score(w);
        if best.is_none_or(|(_, b)| s > b) {
            best = Some((w, s));
        }
    }
    best.map(|(w, _)| w)
}

/// How well `title` names one of `components` (lowercase, deepest first); 0 for no match.
fn title_score(title: &str, components: &[&str]) -> usize {
    let title = title.to_lowercase();
    let segments: Vec<&str> = title
        .split(" - ")
        .flat_map(|s| s.split(" \u{2014} "))
        .map(str::trim)
        .collect();
    let mut best = 0;
    for (depth, component) in components.iter().enumerate() {
        let exact = segments.iter().any(|seg| {
            seg == component
                || seg
                    .strip_prefix(component)
                    .is_some_and(|rest| rest.starts_with(" ("))
        });
        let score = if exact {
            20 - depth
        } else if title.contains(component) {
            10 - depth
        } else {
            0
        };
        best = best.max(score);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(u32, u32, &str)]) -> ProcTable {
        rows.iter()
            .map(|(pid, ppid, exe)| {
                (
                    *pid,
                    Proc {
                        ppid: *ppid,
                        exe: exe.to_string(),
                    },
                )
            })
            .collect()
    }

    fn window(hwnd: isize, pid: u32, title: &str, class: &str) -> WindowInfo {
        WindowInfo {
            hwnd,
            pid,
            title: title.to_string(),
            class: class.to_string(),
            minimized: false,
        }
    }

    fn never_stale(_: u32, _: u32) -> bool {
        false
    }

    #[test]
    fn chain_walks_up_to_the_root() {
        let procs = table(&[
            (10, 8, "claude.exe"),
            (8, 4, "pwsh.exe"),
            (4, 0, "explorer.exe"),
        ]);
        let chain = walk_chain(10, &procs, never_stale);
        let pids: Vec<u32> = chain.iter().map(|l| l.0).collect();
        assert_eq!(pids, vec![10, 8, 4]);
        assert_eq!(chain[1].2, "pwsh.exe");
    }

    #[test]
    fn chain_of_a_missing_pid_is_empty() {
        let procs = table(&[(10, 8, "claude.exe")]);
        assert!(walk_chain(99, &procs, never_stale).is_empty());
    }

    #[test]
    fn chain_stops_at_a_dead_parent() {
        let procs = table(&[(10, 8, "claude.exe")]);
        let chain = walk_chain(10, &procs, never_stale);
        assert_eq!(chain, vec![(10, 8, "claude.exe".to_string())]);
    }

    #[test]
    fn chain_survives_cycles() {
        let procs = table(&[(10, 8, "a.exe"), (8, 10, "b.exe")]);
        assert_eq!(walk_chain(10, &procs, never_stale).len(), 2);
        let procs = table(&[(10, 10, "a.exe")]);
        assert_eq!(walk_chain(10, &procs, never_stale).len(), 1);
    }

    #[test]
    fn chain_is_capped() {
        let rows: Vec<(u32, u32, &str)> = (1..=50).map(|i| (i, i + 1, "x.exe")).collect();
        let procs = table(&rows);
        assert_eq!(walk_chain(1, &procs, never_stale).len(), MAX_ANCESTORS + 1);
    }

    #[test]
    fn chain_stops_at_a_reused_parent_pid() {
        let procs = table(&[(10, 8, "claude.exe"), (8, 4, "notepad.exe")]);
        let chain = walk_chain(10, &procs, |child, parent| child == 10 && parent == 8);
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn describe_marks_processes_with_windows() {
        let chain = vec![
            (10, 8, "claude.exe".to_string()),
            (8, 4, "Code.exe".to_string()),
        ];
        let windows = vec![window(1, 8, "x - Visual Studio Code", "Chrome_WidgetWin_1")];
        let rows = describe_chain(&chain, &windows);
        assert!(!rows[0].has_window);
        assert!(rows[1].has_window);
        assert_eq!(rows[1].host(), HostKind::VsCode);
    }

    #[test]
    fn visible_classic_console_is_precise() {
        let procs = table(&[
            (10, 8, "claude.exe"),
            (8, 4, "cmd.exe"),
            (12, 8, "conhost.exe"),
        ]);
        let chain = walk_chain(10, &procs, never_stale);
        let windows = vec![window(
            77,
            12,
            "Command Prompt - claude",
            CONSOLE_WINDOW_CLASS,
        )];
        let console = ConsoleProbe {
            hwnd: 77,
            owner: None,
        };
        let found = locate(
            &chain,
            &procs,
            &windows,
            Some(&console),
            &FocusRequest::new(10),
        )
        .unwrap();
        assert_eq!(found.route, Route::ConsoleWindow);
        assert_eq!(found.outcome, FocusOutcome::Precise);
        assert_eq!(found.window.hwnd, 77);
    }

    #[test]
    fn pseudo_console_owner_is_the_terminal_window() {
        let procs = table(&[
            (10, 8, "claude.exe"),
            (8, 6, "pwsh.exe"),
            (6, 4, "WindowsTerminal.exe"),
            (4, 0, "explorer.exe"),
        ]);
        let chain = walk_chain(10, &procs, never_stale);
        // Two terminal windows from the same process; the pseudo console's owner decides.
        let windows = vec![
            window(50, 6, "PowerShell", "CASCADIA_HOSTING_WINDOW_CLASS"),
            window(51, 6, "claude", "CASCADIA_HOSTING_WINDOW_CLASS"),
        ];
        let console = ConsoleProbe {
            hwnd: 99,
            owner: Some(51),
        };
        let found = locate(
            &chain,
            &procs,
            &windows,
            Some(&console),
            &FocusRequest::new(10),
        )
        .unwrap();
        assert_eq!(found.route, Route::PseudoConsoleOwner);
        assert_eq!(found.outcome, FocusOutcome::App);
        assert_eq!(found.window.hwnd, 51);
    }

    #[test]
    fn windows_terminal_ancestor_is_app_without_a_console_probe() {
        let procs = table(&[
            (10, 8, "claude.exe"),
            (8, 6, "pwsh.exe"),
            (6, 4, "WindowsTerminal.exe"),
        ]);
        let chain = walk_chain(10, &procs, never_stale);
        let windows = vec![window(50, 6, "claude", "CASCADIA_HOSTING_WINDOW_CLASS")];
        let found = locate(&chain, &procs, &windows, None, &FocusRequest::new(10)).unwrap();
        assert_eq!(found.route, Route::Ancestor);
        assert_eq!(found.outcome, FocusOutcome::App);
    }

    #[test]
    fn vscode_picks_the_window_naming_the_cwd() {
        let procs = table(&[
            (10, 9, "claude.exe"),
            (9, 3, "Code.exe"),
            (3, 2, "Code.exe"),
            (2, 0, "explorer.exe"),
        ]);
        let chain = walk_chain(10, &procs, never_stale);
        let windows = vec![
            window(
                30,
                3,
                "main.rs - other-repo - Visual Studio Code",
                "Chrome_WidgetWin_1",
            ),
            window(
                31,
                3,
                "lib.rs - my-repo - Visual Studio Code",
                "Chrome_WidgetWin_1",
            ),
        ];
        // A visible console must not win over VS Code for an extension session.
        let console = ConsoleProbe {
            hwnd: 30,
            owner: None,
        };
        let request = FocusRequest {
            pid: 10,
            entrypoint: Some(ENTRYPOINT_VSCODE),
            cwd: Some(r"C:\Users\me\Projects\my-repo"),
        };
        let found = locate(&chain, &procs, &windows, Some(&console), &request).unwrap();
        assert_eq!(found.route, Route::Ancestor);
        assert_eq!(found.outcome, FocusOutcome::App);
        assert_eq!(found.window.hwnd, 31);
    }

    #[test]
    fn cwd_subdirectory_still_matches_the_workspace() {
        let a = window(1, 3, "a - other - Visual Studio Code", "c");
        let b = window(2, 3, "b - my-repo - Visual Studio Code", "c");
        let best = best_for_cwd(&[&a, &b], Some(r"C:\p\my-repo\crates\core")).unwrap();
        assert_eq!(best.hwnd, 2);
        // No hint: the topmost window.
        assert_eq!(best_for_cwd(&[&a, &b], None).unwrap().hwnd, 1);
        assert!(best_for_cwd(&[], None).is_none());
    }

    #[test]
    fn exact_title_segment_beats_a_longer_folder_name() {
        let longer = window(1, 3, "Plan - acme-dashboard-next - Visual Studio Code", "c");
        let shorter = window(
            2,
            3,
            "Chart browser e\u{2026} - acme-dashboard - Visual Studio Code",
            "c",
        );
        let other = window(3, 3, "Helper - desktop-switcher - Visual Studio Code", "c");
        let all = [&longer, &shorter, &other];
        let cwd = r"c:\Users\ada\OneDrive - Acme Inc\Projects\acme-dashboard";
        assert_eq!(best_for_cwd(&all, Some(cwd)).unwrap().hwnd, 2);
        let cwd = r"c:\Users\me\Projects\acme-dashboard-next";
        assert_eq!(best_for_cwd(&all, Some(cwd)).unwrap().hwnd, 1);
        let cwd = r"c:\Users\me\Projects\desktop-switcher";
        assert_eq!(best_for_cwd(&all, Some(cwd)).unwrap().hwnd, 3);
        let workspace = window(4, 3, "x - my-repo (Workspace) - Visual Studio Code", "c");
        assert_eq!(title_score(&workspace.title, &["my-repo"]), 20);
    }

    #[test]
    fn console_host_child_of_a_shell_is_precise() {
        // claude started from a cmd window: the console window belongs to the
        // conhost that cmd spawned, which is a sibling of claude, not an ancestor.
        let procs = table(&[
            (10, 8, "claude.exe"),
            (8, 4, "cmd.exe"),
            (12, 8, "conhost.exe"),
            (4, 0, "explorer.exe"),
        ]);
        let chain = walk_chain(10, &procs, never_stale);
        let windows = vec![
            window(5, 4, "File Explorer", "CabinetWClass"),
            window(77, 12, "Command Prompt - claude", CONSOLE_WINDOW_CLASS),
        ];
        let found = locate(&chain, &procs, &windows, None, &FocusRequest::new(10)).unwrap();
        assert_eq!(found.route, Route::ConsoleHostChild);
        assert_eq!(found.outcome, FocusOutcome::Precise);
        assert_eq!(found.window.hwnd, 77);
    }

    #[test]
    fn explorer_windows_are_never_chosen() {
        let procs = table(&[(10, 4, "claude.exe"), (4, 0, "explorer.exe")]);
        let chain = walk_chain(10, &procs, never_stale);
        let windows = vec![window(5, 4, "File Explorer", "CabinetWClass")];
        assert!(locate(&chain, &procs, &windows, None, &FocusRequest::new(10)).is_none());
    }

    #[test]
    fn nothing_found_without_windows() {
        let procs = table(&[(10, 8, "claude.exe"), (8, 0, "node.exe")]);
        let chain = walk_chain(10, &procs, never_stale);
        assert!(locate(&chain, &procs, &[], None, &FocusRequest::new(10)).is_none());
    }

    #[test]
    fn unknown_gui_host_is_app() {
        let procs = table(&[(10, 8, "claude.exe"), (8, 0, "idea64.exe")]);
        let chain = walk_chain(10, &procs, never_stale);
        let windows = vec![window(9, 8, "project - IntelliJ IDEA", "SunAwtFrame")];
        let found = locate(&chain, &procs, &windows, None, &FocusRequest::new(10)).unwrap();
        assert_eq!(found.outcome, FocusOutcome::App);
    }
}
