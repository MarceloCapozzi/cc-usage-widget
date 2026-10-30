//! Opening a link in the default browser (the tray's and the usage view's
//! "Open claude.ai/settings/usage").

/// Opens an `https://` link in the default browser; anything else is
/// refused. Through `ShellExecuteW`, not `cmd /C start`: cmd parses its
/// command line again, so a `&` or a `|` in a URL would run as a command.
/// The links are constants today, and this keeps it safe if one ever is
/// not. It runs on its own thread, which ShellExecute wants with COM
/// initialised, so a slow browser start never stalls the tray.
pub(crate) fn open_url(url: &str) {
    if !is_https_url(url) {
        tracing::warn!("not opening {url:?}: only https links are opened");
        return;
    }
    let url = url.to_string();
    std::thread::spawn(move || shell_open(&url));
}

/// `https://` followed by a host, with no spaces, quotes or control
/// characters anywhere.
fn is_https_url(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

#[cfg(windows)]
fn shell_open(url: &str) {
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (verb, file) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call;
    // the null pointers are the documented "none" for the window, the
    // parameters and the directory. COM is released only if this call
    // initialised it.
    let code = unsafe {
        let com = CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
        if com >= 0 {
            CoUninitialize();
        }
        result as isize
    };
    // Values up to 32 are errors.
    if code <= 32 {
        tracing::warn!("could not open {url} (ShellExecute code {code})");
    }
}

#[cfg(not(windows))]
fn shell_open(url: &str) {
    tracing::warn!("opening links is only supported on Windows ({url})");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_https_links_are_opened() {
        assert!(is_https_url("https://claude.ai/settings/usage"));
        assert!(!is_https_url("http://claude.ai/settings/usage"));
        assert!(!is_https_url("file:///C:/Windows/System32/calc.exe"));
        assert!(!is_https_url(r"C:\Windows\System32\calc.exe"));
        assert!(!is_https_url("https://"));
        assert!(!is_https_url("https:///x"));
        assert!(!is_https_url("https://claude.ai/ & calc"));
        assert!(!is_https_url("https://claude.ai/\"x"));
        assert!(!is_https_url("https://claude.ai/\nx"));
    }
}
