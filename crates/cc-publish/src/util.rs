//! Small helpers shared by the subcommands: clock, time formatting, atomic
//! file writes and Unix permission bits.

use std::path::Path;

/// Current time as Unix seconds with a fractional part.
pub use cc_local::now_secs;

/// Days since 1970-01-01 to a civil (year, month, day), proleptic Gregorian.
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix seconds as `YYYY-MM-DD HH:MM:SS UTC`.
pub fn format_utc(secs: f64) -> String {
    let secs = secs.floor() as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Unix seconds as `YYYYMMDD` (UTC), used to name new keys.
pub fn ymd(secs: f64) -> String {
    let (y, m, d) = civil_from_days((secs.floor() as i64).div_euclid(86_400));
    format!("{y:04}{m:02}{d:02}")
}

/// A duration in seconds as a short human string: `42 s`, `3 min`, `2 h`.
pub fn ago(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0..=119 => format!("{s} s"),
        120..=7199 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}

/// Writes `contents` to a temporary file next to `path` and renames it into
/// place, so readers never see a partial file: [`cc_core::fsutil::write_atomic`].
/// The temporary name is unique per call, because two threads of one process
/// may write the same file (the network task and, when it dies, the
/// gathering thread).
pub use cc_core::fsutil::write_atomic;

/// The permission bits of a file (`mode & 0o777`) on Unix; `None` elsewhere
/// or when the file cannot be inspected.
pub fn unix_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Whether the process `pid` still runs: [`cc_local::process::pid_alive`].
/// `Some(false)` only when the OS says it has exited or never existed; `None`
/// when that cannot be told (pid 0, no permission, an OS other than Windows
/// and Linux).
///
/// A publisher started by a scheduled task can be killed without a chance to
/// mark its state file stopped (ending the task, a forced stop), and one
/// under systemd can crash: the next one, started right away, must not wait
/// 30 s for that file to go stale.
pub fn pid_alive(pid: u32) -> Option<bool> {
    cc_local::process::pid_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_alive_knows_this_process_and_an_exited_one() {
        assert_eq!(pid_alive(0), None);
        if cfg!(any(windows, target_os = "linux")) {
            assert_eq!(pid_alive(std::process::id()), Some(true));
            let mut child =
                std::process::Command::new(if cfg!(windows) { "cmd.exe" } else { "true" })
                    .args(if cfg!(windows) {
                        &["/C", "exit 0"][..]
                    } else {
                        &[][..]
                    })
                    .spawn()
                    .unwrap();
            let pid = child.id();
            child.wait().unwrap();
            assert_eq!(pid_alive(pid), Some(false), "exited (handle still open)");
        } else {
            assert_eq!(pid_alive(std::process::id()), None, "cannot tell");
        }
    }

    #[test]
    fn utc_formatting_matches_known_dates() {
        assert_eq!(format_utc(0.0), "1970-01-01 00:00:00 UTC");
        // 2026-10-13 12:34:56 UTC
        assert_eq!(format_utc(1_791_894_896.7), "2026-10-13 12:34:56 UTC");
        assert_eq!(ymd(1_791_894_896.0), "20261013");
        assert_eq!(format_utc(951_782_400.0), "2000-02-29 00:00:00 UTC");
    }

    #[test]
    fn ago_picks_a_unit() {
        assert_eq!(ago(5.2), "5 s");
        assert_eq!(ago(600.0), "10 min");
        assert_eq!(ago(7200.0), "2 h");
        assert_eq!(ago(-3.0), "0 s");
    }
}
