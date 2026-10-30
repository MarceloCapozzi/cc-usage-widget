//! Diagnostic probe for cc-focus.
//!
//! ```text
//! cargo run -p cc-focus --example focus_probe -- <pid> [--activate] [--detach]
//!     [--entrypoint <name>] [--cwd <dir>]
//! ```
//!
//! Prints the parent chain of `<pid>` and what a focus would do. Only with
//! `--activate` does it actually bring the window to the front.
//!
//! `--detach` re-runs the probe as a process without a console (as the GUI
//! widget runs), so the console-attach route is exercised too; a console
//! program like this one otherwise skips it.

use cc_focus::{describe, focus_with, preview, FocusRequest};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut pid: Option<u32> = None;
    let mut activate = false;
    let mut detach = false;
    let mut entrypoint: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut forwarded: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--activate" => activate = true,
            "--detach" => {
                detach = true;
                continue;
            }
            "--entrypoint" => entrypoint = args.next(),
            "--cwd" => cwd = args.next(),
            other => match other.parse() {
                Ok(p) => pid = Some(p),
                Err(_) => usage(&format!("unexpected argument: {other}")),
            },
        }
        forwarded.push(arg.clone());
        if arg == "--entrypoint" {
            forwarded.extend(entrypoint.clone());
        } else if arg == "--cwd" {
            forwarded.extend(cwd.clone());
        }
    }
    let Some(pid) = pid else {
        usage("missing <pid>")
    };
    if detach {
        rerun_detached(&forwarded);
    }

    let chain = describe(pid);
    if chain.is_empty() {
        println!("process {pid}: not found (or not on Windows)");
    }
    println!(
        "{:>7}  {:>7}  {:<6}  {:<16}  exe",
        "pid", "parent", "window", "host"
    );
    for p in &chain {
        println!(
            "{:>7}  {:>7}  {:<6}  {:<16}  {}",
            p.pid,
            p.parent_pid,
            if p.has_window { "yes" } else { "no" },
            format!("{:?}", p.host()),
            p.exe
        );
    }

    let request = FocusRequest {
        pid,
        entrypoint: entrypoint.as_deref(),
        cwd: cwd.as_deref(),
    };
    let report = if activate {
        focus_with(&request)
    } else {
        preview(&request)
    };
    println!();
    println!(
        "{}: {}",
        if activate { "focus" } else { "preview" },
        report.outcome.as_str()
    );
    if let Some(route) = report.route {
        println!("route:     {route:?}");
    }
    if let Some(w) = &report.window {
        println!(
            "window:    0x{:x} pid {} ({}) class {:?} title {:?}{}",
            w.hwnd,
            w.pid,
            report.window_exe.as_deref().unwrap_or("?"),
            w.class,
            w.title,
            if w.minimized { " [minimised]" } else { "" }
        );
    }
    if let Some(technique) = report.technique {
        println!(
            "technique: {technique:?} (activated: {})",
            report.activated()
        );
    }
}

/// Runs this probe again without a console and relays its output.
fn rerun_detached(args: &[String]) -> ! {
    let exe = std::env::current_exe().expect("own executable path");
    let mut command = std::process::Command::new(exe);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(DETACHED_PROCESS);
    }
    let output = command.output().expect("run detached probe");
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    std::process::exit(output.status.code().unwrap_or(1));
}

fn usage(problem: &str) -> ! {
    eprintln!("{problem}");
    eprintln!(
        "usage: focus_probe <pid> [--activate] [--detach] [--entrypoint <name>] [--cwd <dir>]"
    );
    std::process::exit(2);
}
