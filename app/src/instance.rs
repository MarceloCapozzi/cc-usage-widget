//! One running app per data folder.
//!
//! Two copies of the app on the same data folder would split the cc-hook
//! spool between them (each deletes the events it reads, so both show wrong
//! session states), each would rewrite `history.json`,
//! `notification-state.json` and `remote-projects.json` from its own copy,
//! and a viewer would open a second NATS connection. So `main.rs` claims a
//! named mutex before anything else, `Local\cc-usage-<hash>` ([`key`], the
//! hash of the data folder): a second copy finds it taken, asks the first
//! to show its widget through a named event (`Local\cc-usage-<hash>-show`,
//! [`ask_to_show`]) and exits. The hash keeps an instance with its own
//! `CC_USAGE_DATA_DIR` apart from the installed one.
//!
//! The app's own restart (a role switch, a new machine name) lets go of the
//! mutex first ([`release`]), so the process it starts is not taken for a
//! second copy.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// The name both kernel objects start with: `cc-usage-<16 hex digits>`,
/// the FNV-1a hash of `data_dir` spelled the way Windows compares paths
/// (case and separators ignored, no trailing separator), so the same
/// folder written two ways is one instance. Stable across builds, unlike
/// std's hasher, so an older and a newer copy agree.
pub fn key(data_dir: &Path) -> String {
    let spelled = data_dir
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in spelled.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("cc-usage-{hash:016x}")
}

/// What [`claim`] found.
pub enum Start {
    /// No other copy runs on this data folder: this one is the app.
    First(Instance),
    /// Another copy holds the data folder.
    Running,
}

/// The mutex and the show event of the running app, held for its life.
pub struct Instance {
    /// Held for its drop: closing the last handle frees the name.
    #[cfg(windows)]
    _mutex: Option<win::Handle>,
    #[cfg(windows)]
    show: Option<win::Handle>,
}

/// The waiting side of the show event: [`ShowRequests::wait`] returns each
/// time a second copy asks the app to show its widget.
pub struct ShowRequests {
    #[cfg(windows)]
    event: win::Handle,
}

/// Claims the data folder named by `key` (see [`key`]). A mutex that cannot
/// be created at all counts as no other copy: the app starts unguarded
/// rather than not at all.
pub fn claim(key: &str) -> Start {
    #[cfg(windows)]
    {
        win::claim(key)
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        Start::First(Instance {})
    }
}

/// From a second copy: asks the app holding `key` to show its widget, and
/// lets it take the foreground (this copy was just started by the user, so
/// it may hand that over). Whether the request was delivered.
pub fn ask_to_show(key: &str) -> bool {
    #[cfg(windows)]
    {
        win::ask_to_show(key)
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        false
    }
}

impl Instance {
    /// A handle on the show event for a waiting thread; `None` when the
    /// event could not be created.
    pub fn show_requests(&self) -> Option<ShowRequests> {
        #[cfg(windows)]
        {
            self.show.as_ref()?;
            win::show_requests(self)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

impl ShowRequests {
    /// Blocks until a second copy asks to show the widget (or `timeout`
    /// passes, when given); whether it asked.
    pub fn wait(&self, timeout: Option<Duration>) -> bool {
        #[cfg(windows)]
        {
            win::wait(&self.event, timeout)
        }
        #[cfg(not(windows))]
        {
            let _ = timeout;
            false
        }
    }
}

/// The running app's [`Instance`], held by [`hold`] until [`release`].
static HELD: Mutex<Option<Instance>> = Mutex::new(None);

/// Keeps `instance` for the life of the process.
pub fn hold(instance: Instance) {
    *HELD.lock().unwrap_or_else(|e| e.into_inner()) = Some(instance);
}

/// Lets go of the held instance, so a new process (the app's restart) can
/// claim the data folder.
pub fn release() {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).take();
}

#[cfg(windows)]
mod win {
    use super::{Instance, ShowRequests, Start};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
        INFINITE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};

    /// A kernel handle, closed on drop. Kernel handles may be used and
    /// closed from any thread.
    pub(super) struct Handle(HANDLE);

    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle is valid (never built from a null one) and
            // owned by this value alone.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn mutex_name(key: &str) -> Vec<u16> {
        wide(&format!("Local\\{key}"))
    }

    fn event_name(key: &str) -> Vec<u16> {
        wide(&format!("Local\\{key}-show"))
    }

    /// The show event, created or opened: auto-reset, so one request wakes
    /// the waiting thread once.
    fn create_event(key: &str) -> Option<Handle> {
        let name = event_name(key);
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the
        // call; null attributes are allowed.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        (!handle.is_null()).then_some(Handle(handle))
    }

    pub(super) fn claim(key: &str) -> Start {
        let name = mutex_name(key);
        // SAFETY: as in `create_event`. The last error is read right after
        // the call, on the same thread.
        let (handle, existed) = unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            (handle, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        if handle.is_null() {
            return Start::First(Instance {
                _mutex: None,
                show: None,
            });
        }
        let mutex = Handle(handle);
        if existed {
            return Start::Running;
        }
        Start::First(Instance {
            _mutex: Some(mutex),
            show: create_event(key),
        })
    }

    pub(super) fn ask_to_show(key: &str) -> bool {
        let name = event_name(key);
        // SAFETY: as in `create_event`.
        let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if handle.is_null() {
            return false;
        }
        let event = Handle(handle);
        // SAFETY: plain calls on a valid handle.
        unsafe {
            AllowSetForegroundWindow(ASFW_ANY);
            SetEvent(event.0) != 0
        }
    }

    pub(super) fn show_requests(instance: &Instance) -> Option<ShowRequests> {
        let show = instance.show.as_ref()?;
        let mut copy: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a valid handle within this process; `copy` is
        // written only on success.
        let ok = unsafe {
            use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
            use windows_sys::Win32::System::Threading::GetCurrentProcess;
            let process = GetCurrentProcess();
            DuplicateHandle(
                process,
                show.0,
                process,
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        (ok != 0 && !copy.is_null()).then(|| ShowRequests {
            event: Handle(copy),
        })
    }

    pub(super) fn wait(event: &Handle, timeout: Option<Duration>) -> bool {
        let ms = timeout.map_or(INFINITE, |t| {
            u32::try_from(t.as_millis()).unwrap_or(INFINITE - 1)
        });
        // SAFETY: a plain wait on a valid handle.
        unsafe { WaitForSingleObject(event.0, ms) == WAIT_OBJECT_0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_key_ignores_how_the_folder_is_spelled() {
        let a = key(&PathBuf::from(r"C:\Users\me\AppData\Local\cc-usage"));
        assert_eq!(
            a,
            key(&PathBuf::from(r"c:\users\ME\AppData\Local\cc-usage\"))
        );
        assert_eq!(a, key(&PathBuf::from("C:/Users/me/AppData/Local/cc-usage")));
        assert_ne!(a, key(&PathBuf::from(r"C:\Temp\cc-usage-test")));
        assert!(a.starts_with("cc-usage-") && a.len() == "cc-usage-".len() + 16);
    }

    /// A key no real instance uses: the data folder of this test run.
    fn test_key(name: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        key(&PathBuf::from(format!(
            r"C:\cc-usage-instance-test\{name}-{}-{nanos}",
            std::process::id()
        )))
    }

    #[cfg(windows)]
    #[test]
    fn a_second_copy_finds_the_first_and_asks_it_to_show() {
        let key = test_key("second");
        let Start::First(first) = claim(&key) else {
            panic!("nothing else holds a fresh key");
        };
        let requests = first.show_requests().expect("the show event exists");
        assert!(
            !requests.wait(Some(Duration::from_millis(10))),
            "nobody asked yet"
        );

        assert!(matches!(claim(&key), Start::Running));
        assert!(ask_to_show(&key));
        assert!(requests.wait(Some(Duration::from_secs(5))));
        // Auto-reset: one request, one wake.
        assert!(!requests.wait(Some(Duration::from_millis(10))));

        // Once the first lets go (exit, or its own restart), a new copy is
        // the app again.
        drop(first);
        assert!(matches!(claim(&key), Start::First(_)));
    }

    #[cfg(windows)]
    #[test]
    fn other_data_folders_are_other_instances() {
        let (a, b) = (test_key("a"), test_key("b"));
        let held = claim(&a);
        assert!(matches!(held, Start::First(_)));
        assert!(matches!(claim(&b), Start::First(_)));
        assert!(
            !ask_to_show(&test_key("nobody")),
            "no app holds that folder"
        );
    }
}
