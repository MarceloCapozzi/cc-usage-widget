//! Brings the window that owns a Claude Code session to the front (Windows).
//!
//! A `claude` process has no window of its own: it runs inside a console, a
//! terminal or an editor. When the user clicks a session row in the widget,
//! this crate finds the window that hosts that process and activates it.
//!
//! How a window is located, in order:
//!
//! 1. **The session's console.** The process attaches to the session's console
//!    for a moment (`AttachConsole` + `GetConsoleWindow` + `FreeConsole`). A
//!    visible classic console window (conhost) is the session's own window, so
//!    focus is [`FocusOutcome::Precise`]. Under ConPTY (Windows Terminal) the
//!    console window is a hidden pseudo window whose owner is the hosting
//!    terminal window; that owner is used, with [`FocusOutcome::App`]. This
//!    route is skipped when the calling process has a console of its own (a
//!    console binary such as the `focus_probe` example), because attaching
//!    would mean dropping it; the GUI widget has none.
//! 2. **The parent chain.** Walking up to [`MAX_ANCESTORS`] parents with a
//!    Toolhelp32 snapshot, the first ancestor that owns a visible, unowned,
//!    titled top-level window wins (a console host spawned by a shell in the
//!    chain also counts). VS Code and Windows Terminal give
//!    [`FocusOutcome::App`]: the right application window, but neither the
//!    editor panel nor the terminal tab can be selected from outside. When an
//!    application has several windows, the one whose title mentions the
//!    session's working directory is preferred.
//! 3. Otherwise [`FocusOutcome::NotFound`].
//!
//! Activation restores minimised windows and calls `SetForegroundWindow`.
//! Windows refuses foreground changes requested by background processes, so
//! the documented workarounds are tried in turn and the one that worked is
//! reported as a [`Technique`]: a direct call, `AttachThreadInput` to the
//! current foreground thread, and finally the synthetic ALT key press. If all
//! fail, the taskbar button flashes instead. Call [`focus`] while handling the
//! user's click, so the widget still holds the foreground rights that input
//! grants; nothing can be activated while the workstation is locked.
//!
//! On other platforms everything compiles; [`focus`] returns
//! [`FocusOutcome::Unsupported`] and [`describe`] returns an empty chain.
//!
//! Use [`preview`] for the row tooltip ("focus will be precise"): it locates the
//! window exactly as [`focus_with`] would, without activating anything.

use serde::Serialize;

#[cfg_attr(not(windows), allow(dead_code))]
mod plan;
#[cfg(windows)]
mod win;

/// How many parents above the session process are inspected.
pub const MAX_ANCESTORS: usize = 8;

/// The entrypoint Claude Code records for sessions started by the VS Code extension.
pub const ENTRYPOINT_VSCODE: &str = "claude-vscode";

/// The class name of classic console windows drawn by conhost.
pub const CONSOLE_WINDOW_CLASS: &str = "ConsoleWindowClass";

/// The result of a focus attempt, or of a [`preview`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FocusOutcome {
    /// A window belonging to the session's own console or host was located
    /// (and, for [`focus`], activated): the user lands on the session itself.
    Precise,
    /// The hosting application was located (and activated), e.g. the VS Code
    /// or Windows Terminal window, but not the specific panel or tab.
    App,
    /// No window could be associated with the process.
    NotFound,
    /// Focus is not implemented on this platform.
    Unsupported,
}

impl FocusOutcome {
    /// A stable camelCase name, for logs and the frontend (same as the serde form).
    pub fn as_str(self) -> &'static str {
        match self {
            FocusOutcome::Precise => "precise",
            FocusOutcome::App => "app",
            FocusOutcome::NotFound => "notFound",
            FocusOutcome::Unsupported => "unsupported",
        }
    }

    /// True when a window was located, precise or not.
    pub fn found(self) -> bool {
        matches!(self, FocusOutcome::Precise | FocusOutcome::App)
    }
}

/// Which foreground workaround brought the window to the front.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Technique {
    /// The window was already the foreground window (after restoring it if minimised).
    AlreadyForeground,
    /// A plain `SetForegroundWindow` succeeded (the caller had foreground rights).
    Direct,
    /// `SetForegroundWindow` succeeded while attached to the foreground thread's input.
    AttachThreadInput,
    /// `SetForegroundWindow` succeeded after a synthetic ALT key press.
    AltKey,
    /// Every technique was refused; the taskbar button was flashed instead.
    Failed,
}

/// How the window was associated with the session process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    /// The visible classic console window the session is attached to.
    ConsoleWindow,
    /// The window that owns the session's hidden pseudo console (ConPTY host).
    PseudoConsoleOwner,
    /// A window owned by a process in the parent chain.
    Ancestor,
    /// A console window drawn by a console host spawned by a process in the chain.
    ConsoleHostChild,
}

/// What kind of program a process is, as far as focusing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKind {
    /// VS Code, VS Code Insiders, or a fork (Cursor, Windsurf, VSCodium).
    VsCode,
    /// Windows Terminal: one process for many windows and tabs.
    WindowsTerminal,
    /// `conhost.exe` or `OpenConsole.exe`, the classic console window host.
    ConsoleHost,
    /// A command shell: cmd, PowerShell, bash, ...
    Shell,
    /// A terminal emulator that shows one session per window (mintty, Alacritty).
    Terminal,
    /// A process where the walk stops (Explorer, services): its windows are never the session's.
    Boundary,
    /// Anything else, including the `claude` process itself.
    Other,
}

impl HostKind {
    /// Classifies an executable file name such as `Code.exe` (case-insensitive).
    pub fn from_exe(exe: &str) -> HostKind {
        let exe = exe.to_ascii_lowercase();
        let exe = exe.strip_suffix(".exe").unwrap_or(&exe);
        match exe {
            "code" | "code - insiders" | "cursor" | "windsurf" | "vscodium" | "codium" => {
                HostKind::VsCode
            }
            "windowsterminal" => HostKind::WindowsTerminal,
            "conhost" | "openconsole" => HostKind::ConsoleHost,
            "cmd" | "pwsh" | "powershell" | "bash" | "sh" | "zsh" | "fish" | "nu" | "wsl"
            | "wslhost" => HostKind::Shell,
            "mintty" | "alacritty" => HostKind::Terminal,
            "explorer" | "services" | "svchost" | "wininit" | "winlogon" | "userinit" | "csrss"
            | "smss" | "system" | "[system process]" | "sihost" | "runtimebroker" => {
                HostKind::Boundary
            }
            _ => HostKind::Other,
        }
    }

    /// The outcome when a window of this kind of process is activated.
    pub fn outcome(self) -> FocusOutcome {
        match self {
            HostKind::ConsoleHost | HostKind::Shell | HostKind::Terminal => FocusOutcome::Precise,
            HostKind::VsCode | HostKind::WindowsTerminal | HostKind::Other => FocusOutcome::App,
            HostKind::Boundary => FocusOutcome::NotFound,
        }
    }
}

/// One process in the parent chain of a session, for diagnostics and tooltips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    /// Process id.
    pub pid: u32,
    /// Parent process id as recorded at creation; 0 when there is none.
    pub parent_pid: u32,
    /// Executable file name, e.g. `Code.exe`.
    pub exe: String,
    /// Whether the process owns a visible, unowned, titled top-level window.
    pub has_window: bool,
}

impl ProcessInfo {
    /// The kind of host this process is.
    pub fn host(&self) -> HostKind {
        HostKind::from_exe(&self.exe)
    }
}

/// A top-level window that could be brought to the front.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    /// The window handle as an integer (an `HWND` on Windows).
    pub hwnd: isize,
    /// The process that owns the window.
    pub pid: u32,
    /// Window title.
    pub title: String,
    /// Window class name, e.g. `ConsoleWindowClass`.
    pub class: String,
    /// Whether the window is minimised.
    pub minimized: bool,
}

/// What to focus: a session's process plus the hints the registry provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FocusRequest<'a> {
    /// The `claude` process id.
    pub pid: u32,
    /// The registry's entrypoint (`claude-vscode`, `cli`, ...), if known.
    pub entrypoint: Option<&'a str>,
    /// The session's working directory, used to pick among several windows of one app.
    pub cwd: Option<&'a str>,
}

impl<'a> FocusRequest<'a> {
    /// A request for a process with no hints.
    pub fn new(pid: u32) -> Self {
        FocusRequest {
            pid,
            entrypoint: None,
            cwd: None,
        }
    }

    /// Builds a request from a registry entry; `None` when the entry has no pid.
    pub fn from_entry(entry: &'a cc_core::session::RegistryEntry) -> Option<Self> {
        Some(FocusRequest {
            pid: entry.pid?,
            entrypoint: entry.entrypoint.as_deref(),
            cwd: entry.cwd.as_deref(),
        })
    }
}

/// The full result of [`focus_with`] or [`preview`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusReport {
    /// What was (or would be) achieved.
    pub outcome: FocusOutcome,
    /// How the window was associated with the session, when one was found.
    pub route: Option<Route>,
    /// The window located, when one was found.
    pub window: Option<WindowInfo>,
    /// The executable that owns the window, when one was found.
    pub window_exe: Option<String>,
    /// The foreground technique that worked; `None` for a preview or when nothing was found.
    pub technique: Option<Technique>,
}

impl FocusReport {
    /// A report with an outcome and nothing located.
    pub fn bare(outcome: FocusOutcome) -> Self {
        FocusReport {
            outcome,
            route: None,
            window: None,
            window_exe: None,
            technique: None,
        }
    }

    /// True when a window was located and it ended up in the foreground.
    pub fn activated(&self) -> bool {
        matches!(
            self.technique,
            Some(
                Technique::AlreadyForeground
                    | Technique::Direct
                    | Technique::AttachThreadInput
                    | Technique::AltKey
            )
        )
    }
}

/// Brings the window hosting the session process `pid` to the front.
///
/// `entrypoint` is the registry's value (`claude-vscode` for the VS Code
/// extension). See [`focus_with`] for the full report and a cwd hint.
pub fn focus(pid: u32, entrypoint: Option<&str>) -> FocusOutcome {
    focus_with(&FocusRequest {
        pid,
        entrypoint,
        cwd: None,
    })
    .outcome
}

/// Locates and activates the session's window, reporting how.
///
/// A missing `entrypoint` or `cwd` is filled from Claude Code's own record of
/// the session (see [`session_hints`]).
pub fn focus_with(request: &FocusRequest<'_>) -> FocusReport {
    run(request, true)
}

/// Locates the session's window exactly as [`focus_with`] would, without
/// activating it. Meant for the tooltip that says whether focus will be precise.
pub fn preview(request: &FocusRequest<'_>) -> FocusReport {
    run(request, false)
}

fn run(request: &FocusRequest<'_>, activate: bool) -> FocusReport {
    #[cfg(windows)]
    {
        let hints = if request.entrypoint.is_none() || request.cwd.is_none() {
            session_hints(request.pid)
        } else {
            SessionHints::default()
        };
        let request = FocusRequest {
            pid: request.pid,
            entrypoint: request.entrypoint.or(hints.entrypoint.as_deref()),
            cwd: request.cwd.or(hints.cwd.as_deref()),
        };
        win::run(&request, activate)
    }
    #[cfg(not(windows))]
    {
        let _ = (request, activate);
        FocusReport::bare(FocusOutcome::Unsupported)
    }
}

/// What Claude Code records about a live session that helps pick its window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionHints {
    /// `entrypoint`, e.g. `claude-vscode` or `cli`.
    pub entrypoint: Option<String>,
    /// The session's working directory.
    pub cwd: Option<String>,
}

/// Reads `entrypoint` and `cwd` from Claude Code's session record,
/// `<config>/sessions/<pid>.json`, where `<config>` is `CLAUDE_CONFIG_DIR` or
/// `~/.claude` ([`cc_core::paths::claude_config_dir`]). Empty when the file
/// is missing or unreadable, or no such folder can be found. Only that JSON
/// file is read; nothing else in the directory is touched.
pub fn session_hints(pid: u32) -> SessionHints {
    cc_core::paths::claude_config_dir()
        .map(|dir| session_hints_in(&dir.join("sessions"), pid))
        .unwrap_or_default()
}

/// [`session_hints`] for an explicit sessions directory.
fn session_hints_in(sessions_dir: &std::path::Path, pid: u32) -> SessionHints {
    /// Session records are a few hundred bytes; anything far larger is not one.
    const MAX_RECORD: u64 = 256 * 1024;
    let path = sessions_dir.join(format!("{pid}.json"));
    let fits = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= MAX_RECORD);
    let Some(text) = fits.then(|| std::fs::read_to_string(&path).ok()).flatten() else {
        return SessionHints::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return SessionHints::default();
    };
    let field = |name: &str| value.get(name).and_then(|v| v.as_str()).map(str::to_owned);
    if value
        .get("pid")
        .and_then(|v| v.as_u64())
        .is_some_and(|p| p != u64::from(pid))
    {
        return SessionHints::default();
    }
    SessionHints {
        entrypoint: field("entrypoint"),
        cwd: field("cwd"),
    }
}

/// The parent chain of `pid`, starting with `pid` itself, up to
/// [`MAX_ANCESTORS`] parents. Empty when the process does not exist or on
/// non-Windows platforms. A stale parent id (reused by a newer process) ends
/// the chain.
pub fn describe(pid: u32) -> Vec<ProcessInfo> {
    #[cfg(windows)]
    {
        let windows = win::top_level_windows();
        let procs = win::process_table();
        plan::describe_chain(&win::chain(pid, &procs), &windows)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Vec::new()
    }
}

/// Every visible, unowned, titled, non-tool top-level window, in Z order
/// (topmost first). Enumerating does not change focus. Empty on non-Windows.
pub fn top_level_windows() -> Vec<WindowInfo> {
    #[cfg(windows)]
    {
        win::top_level_windows()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_kinds_are_case_insensitive() {
        assert_eq!(HostKind::from_exe("Code.exe"), HostKind::VsCode);
        assert_eq!(HostKind::from_exe("Code - Insiders.exe"), HostKind::VsCode);
        assert_eq!(
            HostKind::from_exe("WindowsTerminal.exe"),
            HostKind::WindowsTerminal
        );
        assert_eq!(HostKind::from_exe("OpenConsole.exe"), HostKind::ConsoleHost);
        assert_eq!(HostKind::from_exe("conhost.exe"), HostKind::ConsoleHost);
        assert_eq!(HostKind::from_exe("PWSH.EXE"), HostKind::Shell);
        assert_eq!(HostKind::from_exe("explorer.exe"), HostKind::Boundary);
        assert_eq!(HostKind::from_exe("claude.exe"), HostKind::Other);
    }

    #[test]
    fn outcomes_follow_the_host() {
        assert_eq!(HostKind::VsCode.outcome(), FocusOutcome::App);
        assert_eq!(HostKind::WindowsTerminal.outcome(), FocusOutcome::App);
        assert_eq!(HostKind::ConsoleHost.outcome(), FocusOutcome::Precise);
        assert_eq!(HostKind::Shell.outcome(), FocusOutcome::Precise);
        assert!(FocusOutcome::App.found() && !FocusOutcome::NotFound.found());
        assert_eq!(FocusOutcome::NotFound.as_str(), "notFound");
    }

    #[test]
    fn request_from_registry_entry_needs_a_pid() {
        let json = r#"{"sessionId":"s","pid":42,"cwd":"C:\\p","status":"idle","entrypoint":"claude-vscode"}"#;
        let entry: cc_core::session::RegistryEntry = serde_json::from_str(json).unwrap();
        let req = FocusRequest::from_entry(&entry).unwrap();
        assert_eq!(req.pid, 42);
        assert_eq!(req.entrypoint, Some(ENTRYPOINT_VSCODE));
        assert_eq!(req.cwd, Some("C:\\p"));
        let json = r#"{"sessionId":"s","status":"idle"}"#;
        let entry: cc_core::session::RegistryEntry = serde_json::from_str(json).unwrap();
        assert!(FocusRequest::from_entry(&entry).is_none());
    }

    #[test]
    fn report_serializes_in_camel_case() {
        let report = FocusReport::bare(FocusOutcome::NotFound);
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(r#""outcome":"notFound""#), "{json}");
        assert!(json.contains(r#""windowExe":null"#), "{json}");
        assert!(!report.activated());
    }

    #[test]
    fn describe_starts_with_the_current_process() {
        let me = std::process::id();
        let chain = describe(me);
        if cfg!(windows) {
            assert!(!chain.is_empty());
            assert_eq!(chain[0].pid, me);
            assert!(
                chain[0].exe.to_ascii_lowercase().ends_with(".exe"),
                "{:?}",
                chain[0]
            );
            assert!(chain.len() <= MAX_ANCESTORS + 1);
            for pair in chain.windows(2) {
                assert_eq!(pair[0].parent_pid, pair[1].pid);
            }
        } else {
            assert!(chain.is_empty());
        }
    }

    #[test]
    fn describe_of_a_missing_process_is_empty() {
        // Process ids are multiples of 4 on Windows, so this one never exists.
        assert!(describe(u32::MAX - 2).is_empty());
    }

    #[test]
    fn enumerating_windows_does_not_need_focus() {
        for w in top_level_windows() {
            assert!(!w.title.is_empty());
            assert_ne!(w.hwnd, 0);
        }
    }

    #[test]
    fn preview_never_activates_and_handles_missing_processes() {
        let report = preview(&FocusRequest::new(u32::MAX - 2));
        assert!(report.technique.is_none());
        if cfg!(windows) {
            assert_eq!(report.outcome, FocusOutcome::NotFound);
        } else {
            assert_eq!(report.outcome, FocusOutcome::Unsupported);
        }
    }

    #[test]
    fn preview_of_the_test_process_is_consistent() {
        // The test runner lives in some console or terminal; whatever is found
        // must be reported coherently, and nothing is activated.
        let report = preview(&FocusRequest::new(std::process::id()));
        assert!(report.technique.is_none());
        assert_eq!(report.outcome.found(), report.window.is_some());
        assert_eq!(report.window.is_some(), report.route.is_some());
    }

    fn temp_sessions_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cc-focus-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn session_hints_come_from_the_record() {
        let dir = temp_sessions_dir("hints");
        std::fs::write(
            dir.join("42.json"),
            r#"{"pid":42,"cwd":"C:\\p\\repo","entrypoint":"claude-vscode","unknown":{"x":1}}"#,
        )
        .unwrap();
        let hints = session_hints_in(&dir, 42);
        assert_eq!(hints.cwd.as_deref(), Some(r"C:\p\repo"));
        assert_eq!(hints.entrypoint.as_deref(), Some(ENTRYPOINT_VSCODE));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn session_hints_tolerate_missing_and_broken_records() {
        let dir = temp_sessions_dir("broken");
        assert_eq!(session_hints_in(&dir, 7), SessionHints::default());
        std::fs::write(dir.join("7.json"), "{ not json").unwrap();
        assert_eq!(session_hints_in(&dir, 7), SessionHints::default());
        // A record that names another pid is not trusted.
        std::fs::write(dir.join("8.json"), r#"{"pid":9,"cwd":"C:\\x"}"#).unwrap();
        assert_eq!(session_hints_in(&dir, 8), SessionHints::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn focus_is_unsupported_off_windows() {
        assert_eq!(focus(1, None), FocusOutcome::Unsupported);
    }
}
