//! Helpers for cc-hook's tests: the unit tests (`crate::testutil`) and the
//! integration tests under `tests/`, which include this file with
//! `#[path = "../src/testutil.rs"] mod testutil;`.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::Path;

/// Set to `1` to make a test that needs a shell fail when there is none,
/// instead of passing after printing `skipped: …`.
pub const REQUIRE_SHELL_ENV: &str = "CC_TEST_REQUIRE_SHELL";

/// Set to `1` (CI does) to make every test of the workspace that would skip
/// fail instead; it implies [`REQUIRE_SHELL_ENV`].
pub const REQUIRE_ALL_ENV: &str = "CC_TEST_REQUIRE_ALL";

/// A fresh folder under the system temp folder, removed when dropped, also
/// when the test fails. A removal that fails (a scanner holding a file the
/// test just wrote) is ignored instead of failing a test whose checks passed.
///
/// It dereferences to [`Path`], so `&dir` is a `&Path` and `dir.join(..)`
/// works as on a path.
pub struct TestDir(tempfile::TempDir);

impl TestDir {
    /// A new folder named `cc-hook-<tag>-<random>`.
    pub fn new(tag: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(&format!("cc-hook-{tag}-"))
            .tempdir()
            .expect("create a temp folder");
        Self(dir)
    }
}

impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<OsStr> for TestDir {
    fn as_ref(&self) -> &OsStr {
        self.0.path().as_os_str()
    }
}

/// Reports a test that cannot run because `what` is missing, and returns so
/// the caller can `return`: prints `skipped: <what>` to stderr, or panics
/// when [`REQUIRE_SHELL_ENV`] or [`REQUIRE_ALL_ENV`] is `1`, so CI never
/// reports a test that did not run as passed.
pub fn skip_without_shell(what: &str) {
    for var in [REQUIRE_SHELL_ENV, REQUIRE_ALL_ENV] {
        if std::env::var_os(var).is_some_and(|v| v == "1") {
            panic!("{what}, and {var}=1 requires the shell tests to run");
        }
    }
    eprintln!("skipped: {what}");
}
