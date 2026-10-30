//! "Start with Windows", the checkable tray menu item.
//!
//! The real switch is [`RunLauncher`] over the registry ([`Registry`]),
//! which the app binary (`tray.rs`, `commands.rs`) uses as the [`Launcher`].
//! It only ever touches its own value, named after the app ([`RUN_VALUE`]):
//!
//! - enable: opens the existing `HKCU\Software\Microsoft\Windows\
//!   CurrentVersion\Run` key (never creates it) and sets the `cc-usage`
//!   value to the exe's path **in quotes**; if
//!   `...\Explorer\StartupApproved\Run` exists, it also sets its `cc-usage`
//!   value to "enabled", which undoes a "Disable" from Task Manager's
//!   Startup tab.
//! - disable: deletes the `cc-usage` value from the Run key, nothing else.
//! - is enabled: the Run value exists, whatever it holds, and Task Manager
//!   has not disabled it.
//!
//! The quotes matter: the app installs per user, so the path is under the
//! profile folder, which often has a space in it (`C:\Users\Ada
//! Lovelace\...`); unquoted, Windows would split the command at the space
//! and look for `C:\Users\Ada.exe`, which at best fails and at worst runs
//! whatever someone put there.
//!
//! The menu logic and the launcher's rules are here, behind two traits, so
//! they are tested with fakes: nothing in the tests reads or writes the
//! registry.

use std::path::PathBuf;

/// The value's name under the Run key (and under StartupApproved\Run): the
/// app's product name, the one the plugin used.
pub const RUN_VALUE: &str = "cc-usage";

/// `HKCU\` + this is where Windows looks for per-user programs to start.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// `HKCU\` + this holds Task Manager's per-entry "Enabled"/"Disabled".
pub const STARTUP_APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// What Task Manager writes for an enabled entry: 2, then eight zero bytes
/// where a disabled one has the time it was disabled.
const APPROVED_ENABLED: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// The registry as [`RunLauncher`] needs it: the app's own value under the
/// Run key and under StartupApproved\Run, nothing else.
pub trait RunStore {
    /// The Run value, `None` when there is none. An error when the Run key
    /// cannot be opened.
    fn run_value(&self) -> Result<Option<String>, String>;
    /// Sets the Run value. Fails if the Run key does not exist: it is never
    /// created.
    fn set_run_value(&self, command: &str) -> Result<(), String>;
    /// Deletes the Run value; no value is not an error.
    fn delete_run_value(&self) -> Result<(), String>;
    /// The StartupApproved value's bytes, `None` when the key or the value
    /// is missing.
    fn approved(&self) -> Option<Vec<u8>>;
    /// Sets the StartupApproved value to `bytes`, only if that key exists.
    fn set_approved(&self, bytes: &[u8]) -> Result<(), String>;
}

/// Starting with Windows through the Run key, for the exe at `exe`.
pub struct RunLauncher<S> {
    store: S,
    exe: PathBuf,
}

impl<S: RunStore> RunLauncher<S> {
    pub fn new(store: S, exe: PathBuf) -> Self {
        Self { store, exe }
    }

    /// What the Run value holds: the exe's path in double quotes. A Windows
    /// path cannot contain a `"`, so there is nothing to escape.
    pub fn command(&self) -> String {
        format!("\"{}\"", self.exe.display())
    }
}

impl<S: RunStore> Launcher for RunLauncher<S> {
    fn is_enabled(&self) -> Result<bool, String> {
        let on = self.store.run_value()?.is_some();
        Ok(on
            && self
                .store
                .approved()
                .as_deref()
                .is_none_or(approved_enabled))
    }

    fn enable(&self) -> Result<(), String> {
        if self.exe.as_os_str().is_empty() {
            return Err("the app's own path is unknown".into());
        }
        self.store.set_run_value(&self.command())?;
        self.store.set_approved(&APPROVED_ENABLED)
    }

    fn disable(&self) -> Result<(), String> {
        self.store.delete_run_value()
    }
}

/// Whether Task Manager's bytes say enabled: the last eight are zero (a
/// disabled entry keeps the time it was disabled there). Fewer than eight
/// bytes are not Task Manager's, and do not count as disabled.
fn approved_enabled(bytes: &[u8]) -> bool {
    bytes.len() < 8 || bytes[bytes.len() - 8..].iter().all(|&b| b == 0)
}

/// The real registry, under `HKEY_CURRENT_USER`.
pub struct Registry;

#[cfg(windows)]
impl RunStore for Registry {
    fn run_value(&self) -> Result<Option<String>, String> {
        use winreg::{enums::*, RegKey};
        let run = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .map_err(|e| e.to_string())?;
        match run.get_value::<String, _>(RUN_VALUE) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set_run_value(&self, command: &str) -> Result<(), String> {
        use winreg::{enums::*, RegKey};
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
            .and_then(|run| run.set_value(RUN_VALUE, &command))
            .map_err(|e| e.to_string())
    }

    fn delete_run_value(&self) -> Result<(), String> {
        use winreg::{enums::*, RegKey};
        let run = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
            .map_err(|e| e.to_string())?;
        match run.delete_value(RUN_VALUE) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    fn approved(&self) -> Option<Vec<u8>> {
        use winreg::{enums::*, RegKey};
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(STARTUP_APPROVED_KEY, KEY_READ)
            .ok()?;
        key.get_raw_value(RUN_VALUE).ok().map(|v| v.bytes)
    }

    fn set_approved(&self, bytes: &[u8]) -> Result<(), String> {
        use winreg::{enums::*, RegKey, RegValue};
        // The key may not exist (Task Manager creates it); then there is
        // nothing to undo.
        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(STARTUP_APPROVED_KEY, KEY_SET_VALUE)
        else {
            return Ok(());
        };
        let value = RegValue {
            vtype: REG_BINARY,
            bytes: bytes.to_vec(),
        };
        key.set_raw_value(RUN_VALUE, &value)
            .map_err(|e| e.to_string())
    }
}

/// Elsewhere there is no Run key: the menu item shows off and says why.
#[cfg(not(windows))]
impl RunStore for Registry {
    fn run_value(&self) -> Result<Option<String>, String> {
        Err("starting with the system is only supported on Windows".into())
    }
    fn set_run_value(&self, _: &str) -> Result<(), String> {
        self.run_value().map(|_| ())
    }
    fn delete_run_value(&self) -> Result<(), String> {
        self.run_value().map(|_| ())
    }
    fn approved(&self) -> Option<Vec<u8>> {
        None
    }
    fn set_approved(&self, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

/// The launcher for this process: the real registry and this exe.
pub fn system() -> RunLauncher<Registry> {
    RunLauncher::new(Registry, std::env::current_exe().unwrap_or_default())
}

/// The tray menu item's label (user-facing).
pub const MENU_LABEL: &str = "Start with Windows";

/// Something that can turn starting with Windows on and off.
pub trait Launcher {
    /// Whether the app starts with Windows now.
    fn is_enabled(&self) -> Result<bool, String>;
    /// Makes it start with Windows.
    fn enable(&self) -> Result<(), String>;
    /// Stops it starting with Windows.
    fn disable(&self) -> Result<(), String>;
}

/// The check the menu item shows: the real state, off when it cannot be
/// read.
pub fn checked(launcher: &dyn Launcher) -> bool {
    launcher.is_enabled().unwrap_or(false)
}

/// What a click on the menu item did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toggled {
    /// The state in force afterwards, read back: the check to show.
    pub checked: bool,
    /// Why the change did not happen, if it did not.
    pub error: Option<String>,
}

/// Flips starting with Windows and reports the state really in force
/// afterwards, read back rather than assumed: the menu toggles its own
/// check on every click, so the caller sets it to [`Toggled::checked`].
pub fn toggle(launcher: &dyn Launcher) -> Toggled {
    let was = checked(launcher);
    let result = if was {
        launcher.disable()
    } else {
        launcher.enable()
    };
    let now = launcher.is_enabled();
    let checked = now.as_ref().copied().unwrap_or(false);
    let error = match (result, now) {
        (Err(e), _) => Some(format!(
            "could not {} starting with Windows: {e}",
            if was { "turn off" } else { "turn on" }
        )),
        (Ok(()), Err(e)) => Some(format!("could not read the autostart state: {e}")),
        (Ok(()), Ok(state)) if state == was => {
            Some("the autostart state did not change".to_string())
        }
        (Ok(()), Ok(_)) => None,
    };
    Toggled { checked, error }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A launcher held in memory, with switchable failures.
    #[derive(Default)]
    struct Fake {
        on: Cell<bool>,
        fail_enable: bool,
        fail_disable: bool,
        fail_read: Cell<bool>,
        /// Stuck: enable and disable succeed but change nothing (a policy
        /// putting the value back, say).
        stuck: bool,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Launcher for Fake {
        fn is_enabled(&self) -> Result<bool, String> {
            self.calls.borrow_mut().push("is_enabled");
            if self.fail_read.get() {
                return Err("access denied".into());
            }
            Ok(self.on.get())
        }
        fn enable(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("enable");
            if self.fail_enable {
                return Err("the Run key is missing".into());
            }
            if !self.stuck {
                self.on.set(true);
            }
            Ok(())
        }
        fn disable(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("disable");
            if self.fail_disable {
                return Err("value not found".into());
            }
            if !self.stuck {
                self.on.set(false);
            }
            Ok(())
        }
    }

    #[test]
    fn the_check_shows_the_real_state() {
        let fake = Fake::default();
        assert!(!checked(&fake));
        fake.on.set(true);
        assert!(checked(&fake));
        fake.fail_read.set(true);
        assert!(!checked(&fake), "unreadable counts as off");
    }

    #[test]
    fn a_click_turns_it_on_then_off() {
        let fake = Fake::default();
        assert_eq!(
            toggle(&fake),
            Toggled {
                checked: true,
                error: None
            }
        );
        assert!(fake.on.get());
        assert_eq!(
            toggle(&fake),
            Toggled {
                checked: false,
                error: None
            }
        );
        assert!(!fake.on.get());
        assert_eq!(
            *fake.calls.borrow(),
            [
                "is_enabled",
                "enable",
                "is_enabled",
                "is_enabled",
                "disable",
                "is_enabled"
            ],
            "read, act, read back"
        );
    }

    #[test]
    fn a_failed_enable_leaves_the_check_off_and_says_why() {
        let fake = Fake {
            fail_enable: true,
            ..Default::default()
        };
        let t = toggle(&fake);
        assert!(!t.checked);
        let error = t.error.unwrap();
        assert!(
            error.contains("turn on") && error.contains("Run key is missing"),
            "{error}"
        );
    }

    #[test]
    fn a_failed_disable_leaves_the_check_on() {
        let fake = Fake {
            fail_disable: true,
            ..Default::default()
        };
        fake.on.set(true);
        let t = toggle(&fake);
        assert!(t.checked);
        assert!(t.error.unwrap().contains("turn off"));
    }

    #[test]
    fn a_change_that_does_not_stick_is_reported() {
        let fake = Fake {
            stuck: true,
            ..Default::default()
        };
        let t = toggle(&fake);
        assert!(!t.checked, "the check follows what is in force");
        assert!(t.error.is_some());
    }

    #[test]
    fn an_unreadable_state_counts_as_off_so_a_click_enables() {
        let fake = Fake::default();
        fake.fail_read.set(true);
        let t = toggle(&fake);
        assert_eq!(fake.calls.borrow()[1], "enable");
        assert!(!t.checked);
        assert!(t.error.unwrap().contains("could not read"));
    }

    /// The registry held in memory: the two values, and whether each key
    /// exists.
    struct FakeStore {
        run_key: bool,
        run: RefCell<Option<String>>,
        approved_key: bool,
        approved: RefCell<Option<Vec<u8>>>,
        writes: Cell<usize>,
    }

    impl Default for FakeStore {
        fn default() -> Self {
            Self {
                run_key: true,
                run: RefCell::new(None),
                approved_key: true,
                approved: RefCell::new(None),
                writes: Cell::new(0),
            }
        }
    }

    impl RunStore for &FakeStore {
        fn run_value(&self) -> Result<Option<String>, String> {
            if !self.run_key {
                return Err("the Run key is missing".into());
            }
            Ok(self.run.borrow().clone())
        }
        fn set_run_value(&self, command: &str) -> Result<(), String> {
            if !self.run_key {
                return Err("the Run key is missing".into());
            }
            self.writes.set(self.writes.get() + 1);
            *self.run.borrow_mut() = Some(command.to_string());
            Ok(())
        }
        fn delete_run_value(&self) -> Result<(), String> {
            self.run_value()?;
            *self.run.borrow_mut() = None;
            Ok(())
        }
        fn approved(&self) -> Option<Vec<u8>> {
            self.approved.borrow().clone()
        }
        fn set_approved(&self, bytes: &[u8]) -> Result<(), String> {
            if self.approved_key {
                *self.approved.borrow_mut() = Some(bytes.to_vec());
            }
            Ok(())
        }
    }

    const EXE: &str = r"C:\Users\Ada Lovelace\AppData\Local\cc-usage\cc-usage.exe";

    fn launcher(store: &FakeStore) -> RunLauncher<&FakeStore> {
        RunLauncher::new(store, PathBuf::from(EXE))
    }

    #[test]
    fn enable_writes_the_path_in_quotes() {
        let store = FakeStore::default();
        let l = launcher(&store);
        assert!(!l.is_enabled().unwrap());
        l.enable().unwrap();
        assert_eq!(
            store.run.borrow().as_deref(),
            Some(format!("\"{EXE}\"").as_str())
        );
        assert!(l.is_enabled().unwrap());
        assert_eq!(
            store.approved.borrow().as_deref(),
            Some(&APPROVED_ENABLED[..])
        );
        l.disable().unwrap();
        assert!(!l.is_enabled().unwrap());
        l.disable().unwrap();
    }

    #[test]
    fn the_run_key_is_never_created() {
        let store = FakeStore {
            run_key: false,
            approved_key: false,
            ..Default::default()
        };
        let l = launcher(&store);
        assert!(l.enable().is_err());
        assert!(l.is_enabled().is_err());
        assert!(store.run.borrow().is_none() && store.approved.borrow().is_none());
    }

    /// Any value counts as on; enable writes this exe's path in quotes.
    #[test]
    fn any_value_counts_and_enable_rewrites_it() {
        let store = FakeStore::default();
        *store.run.borrow_mut() = Some(r"D:\dev\target\debug\cc-usage.exe".into());
        let l = launcher(&store);
        assert!(l.is_enabled().unwrap());
        assert_eq!(store.writes.get(), 0, "reading writes nothing");
        l.enable().unwrap();
        assert_eq!(
            store.run.borrow().as_deref(),
            Some(format!("\"{EXE}\"").as_str())
        );
    }

    /// Task Manager's "Disable" wins over the Run value, and enable undoes it.
    #[test]
    fn a_task_manager_disable_is_honoured() {
        let store = FakeStore::default();
        let l = launcher(&store);
        l.enable().unwrap();
        *store.approved.borrow_mut() =
            Some(vec![3, 0, 0, 0, 0x5a, 0x1b, 0x2c, 0x3d, 0x01, 0xd9, 0, 0]);
        assert!(!l.is_enabled().unwrap());
        assert!(toggle(&l).checked, "a click turns it back on");
        assert!(approved_enabled(&[1, 2]), "not Task Manager's");
    }
}
