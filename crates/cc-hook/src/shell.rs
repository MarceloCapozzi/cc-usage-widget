//! Running a command string the way Claude Code runs a shell-form command.
//!
//! Claude Code's documentation (status line, "Windows configuration"; hooks,
//! "Shell form") says a `command` string goes to `sh -c` on macOS and Linux,
//! and on Windows to Git Bash when it is installed, or to PowerShell when it
//! is not. `cc-hook statusline` runs the status line command it replaced the
//! same way, so the original keeps its quoting, `~` expansion, pipes and
//! environment exactly as before:
//!
//! - Unix: `/bin/sh -c <command>`;
//! - Windows with Git Bash: `<git>\bin\bash.exe -c <command>`, where Git Bash is
//!   found like Claude Code finds it: `CLAUDE_CODE_GIT_BASH_PATH` when set and
//!   present, else from the `git.exe` on `PATH` (`<root>\cmd`, `<root>\bin` or
//!   `<root>\mingw64\bin` all lead to `<root>\bin\bash.exe`), else the standard
//!   install folders. `bash.exe` found directly on `PATH` is never used: on
//!   Windows that is often WSL's launcher in `System32`;
//! - Windows without Git Bash: `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe
//!   -NoProfile -NonInteractive -Command <command>` (the full path, so a
//!   `powershell.exe` planted in the working directory or on `PATH` is never
//!   run). A command that starts with a quoted program gets `& ` in front:
//!   to PowerShell a leading quoted string is a value, not a call.
//!
//! [`words`] splits a command string into words the way these shells would
//! for the simple cases cc-hook needs (the program name of a hook or tool
//! command).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The program and leading arguments that run a command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    /// The shell executable.
    pub program: PathBuf,
    /// Arguments before the command string.
    pub args: Vec<OsString>,
}

impl Shell {
    /// A [`Command`] that runs `command` through this shell.
    pub fn command(&self, command: &str) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args).arg(self.prepare(command));
        c
    }

    /// Whether this is Windows PowerShell or PowerShell 7.
    pub fn is_powershell(&self) -> bool {
        self.program
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("powershell") || s.eq_ignore_ascii_case("pwsh"))
    }

    /// The command string as this shell must get it: for PowerShell, `& `
    /// before a command that starts with a quoted program.
    pub fn prepare(&self, command: &str) -> String {
        let trimmed = command.trim_start();
        if self.is_powershell() && (trimmed.starts_with('"') || trimmed.starts_with('\'')) {
            format!("& {trimmed}")
        } else {
            command.to_string()
        }
    }
}

/// Windows PowerShell by full path: `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`,
/// with `C:\Windows` when `SystemRoot` is not set.
pub fn windows_powershell() -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

/// The shell Claude Code would use on this machine.
pub fn claude_code_shell() -> Shell {
    if cfg!(windows) {
        match find_git_bash() {
            Some(bash) => Shell {
                program: bash,
                args: vec!["-c".into()],
            },
            None => Shell {
                program: windows_powershell(),
                args: vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                ],
            },
        }
    } else {
        Shell {
            program: "/bin/sh".into(),
            args: vec!["-c".into()],
        }
    }
}

/// Git Bash's `bash.exe`, looked up like Claude Code does.
pub fn find_git_bash() -> Option<PathBuf> {
    find_git_bash_in(
        std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from),
        std::env::var_os("PATH").as_deref(),
        &standard_git_roots(),
    )
}

fn standard_git_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(var) {
            roots.push(PathBuf::from(base).join("Git"));
        }
    }
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(base).join("Programs").join("Git"));
    }
    roots.push(PathBuf::from(r"C:\Program Files\Git"));
    roots
}

/// The testable core of [`find_git_bash`].
pub fn find_git_bash_in(
    explicit: Option<PathBuf>,
    path_var: Option<&std::ffi::OsStr>,
    roots: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(p) = explicit.filter(|p| p.is_file()) {
        return Some(p);
    }
    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(path_var) {
            if !dir.join("git.exe").is_file() {
                continue;
            }
            // <root>\cmd\git.exe, <root>\bin\git.exe, <root>\mingw64\bin\git.exe
            for root in dir.ancestors().skip(1).take(2) {
                if let Some(bash) = bash_under(root) {
                    return Some(bash);
                }
            }
        }
    }
    roots.iter().find_map(|r| bash_under(r))
}

fn bash_under(root: &Path) -> Option<PathBuf> {
    let bash = root.join("bin").join("bash.exe");
    bash.is_file().then_some(bash)
}

/// The first `max` words of a command string. Double- and single-quoted
/// parts are joined to the word around them without their quotes, and `\'`
/// outside quotes is a literal `'` (so POSIX `'it'\''s'` reads `it's`). Any
/// other backslash is kept: unquoted Windows paths are full of them. This is
/// not a full shell parser; it only has to find program names.
pub fn words(command: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = command.chars().peekable();
    while out.len() < max {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut word = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' | '\'' => {
                    for q in chars.by_ref() {
                        if q == c {
                            break;
                        }
                        word.push(q);
                    }
                }
                '\\' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    word.push('\'');
                }
                c if c.is_whitespace() => break,
                c => word.push(c),
            }
        }
        out.push(word);
    }
    out
}

/// The program a command string runs: its first word, past a PowerShell
/// call operator `&`. Empty for a blank command.
pub fn program_of(command: &str) -> String {
    let mut w = words(command, 2).into_iter();
    match w.next() {
        Some(first) if first == "&" => w.next().unwrap_or_default(),
        Some(first) => first,
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;

    #[test]
    fn words_follow_shell_quoting_for_the_simple_cases() {
        assert_eq!(
            words(r#"  "C:\a b\x.exe" --y z"#, 9),
            [r"C:\a b\x.exe", "--y", "z"]
        );
        assert_eq!(
            words(r"'/tmp/it'\''s/cc-hook' statusline", 9),
            ["/tmp/it's/cc-hook", "statusline"]
        );
        assert_eq!(words(r"C:\tools\x.exe a", 1), [r"C:\tools\x.exe"]);
        assert_eq!(words("a\"b c\"d e", 9), ["ab cd", "e"]);
        assert!(words("   ", 3).is_empty());
        assert_eq!(
            program_of(r#"& "C:/x/cc-hook.exe" statusline"#),
            "C:/x/cc-hook.exe"
        );
        assert_eq!(program_of(""), "");
    }

    #[test]
    fn powershell_gets_a_call_operator_before_a_quoted_program() {
        let ps = Shell {
            program: windows_powershell(),
            args: vec!["-Command".into()],
        };
        assert!(ps.is_powershell());
        assert_eq!(ps.prepare(r#""C:/x/y.exe" --a"#), r#"& "C:/x/y.exe" --a"#);
        assert_eq!(ps.prepare("  'C:/x/y.exe'"), "& 'C:/x/y.exe'");
        assert_eq!(ps.prepare("node x.js"), "node x.js");
        let pwsh = Shell {
            program: PathBuf::from("C:/p/PWSH.EXE"),
            args: vec![],
        };
        assert!(pwsh.is_powershell());
        let bash = Shell {
            program: PathBuf::from("C:/Git/bin/bash.exe"),
            args: vec!["-c".into()],
        };
        assert_eq!(bash.prepare(r#""C:/x/y.exe""#), r#""C:/x/y.exe""#);
        let full = windows_powershell();
        assert!(full.is_absolute() || !cfg!(windows), "{}", full.display());
        assert!(full.ends_with(
            Path::new("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        ));
    }

    fn fake_git(tag: &str) -> TestDir {
        let root = TestDir::new(&format!("shell-{tag}"));
        for d in ["bin", "cmd", "mingw64/bin"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("bin/bash.exe"), b"").unwrap();
        std::fs::write(root.join("cmd/git.exe"), b"").unwrap();
        std::fs::write(root.join("mingw64/bin/git.exe"), b"").unwrap();
        root
    }

    #[test]
    fn git_bash_comes_from_the_explicit_path_then_git_on_path_then_standard_roots() {
        let git = fake_git("find");
        let root = git.to_path_buf();
        let bash = root.join("bin").join("bash.exe");
        let elsewhere = std::env::temp_dir().join("cc-hook-no-such-dir");

        let explicit = root.join("explicit-bash.exe");
        std::fs::write(&explicit, b"").unwrap();
        assert_eq!(
            find_git_bash_in(Some(explicit.clone()), None, &[]),
            Some(explicit)
        );
        // A missing explicit path falls through.
        assert_eq!(
            find_git_bash_in(Some(elsewhere.clone()), None, std::slice::from_ref(&root)),
            Some(bash.clone())
        );

        for dir in [root.join("cmd"), root.join("mingw64").join("bin")] {
            let path = std::env::join_paths([elsewhere.clone(), dir]).unwrap();
            assert_eq!(find_git_bash_in(None, Some(&path), &[]), Some(bash.clone()));
        }
        assert_eq!(
            find_git_bash_in(None, Some(elsewhere.as_os_str()), &[]),
            None
        );
    }

    #[test]
    fn a_bare_bash_on_path_is_not_git_bash() {
        let dir = TestDir::new("shell-wsl");
        std::fs::write(dir.join("bash.exe"), b"").unwrap();
        assert_eq!(find_git_bash_in(None, Some(dir.as_os_str()), &[]), None);
    }
}
