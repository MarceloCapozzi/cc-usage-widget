//! `cc-hook`: the binary Claude Code runs on every hook event, plus the
//! installer that registers it in `settings.json`.
//!
//! Three jobs live here:
//!
//! 1. **Receiver** (`cc-hook` with no arguments, see [`receiver`]). Claude Code
//!    pipes a hook payload as JSON on stdin. The receiver reduces it to a
//!    [`cc_core::events::SpoolEvent`] ([`payload`]), resolves the pid of the
//!    Claude Code process that ran it ([`procs`]), and drops one JSON file into
//!    the spool directory ([`spool`]), written to a dotted temp name and renamed
//!    into place. The receiver never writes to stdout or stderr, never exits
//!    non-zero and never panics on bad input: Claude Code acts on stdout and
//!    shows stderr to the user, so a hook that talks or fails breaks sessions.
//!    With `CC_HOOK_DEBUG` set, diagnostics go to `<data_dir>/cc-hook.log`
//!    ([`debuglog`]).
//!
//! 2. **Installer** (`cc-hook install | uninstall | status`, see [`install`]).
//!    Adds, updates or removes one cc-hook entry per event in
//!    [`cc_core::events::HOOK_EVENTS`] without touching any other tool's hooks
//!    or any other key, keeps key order, takes a timestamped backup, writes via
//!    temp file and rename, verifies the result and restores the backup on
//!    failure.
//!
//! 3. **Status line** (`cc-hook statusline`, see [`statusline`]). Installed in
//!    front of the user's own status line command: writes the exact context
//!    window use of each session to `<data_dir>/live/<session_id>.json`, then
//!    runs the replaced command ([`chain`]) through the shell Claude Code would
//!    use ([`shell`]) with the same stdin, so its output is unchanged.
//!
//! The command-line front end that dispatches between them is [`cli`].

#![warn(missing_docs)]

pub mod chain;
pub mod cli;
pub mod debuglog;
pub mod install;
pub mod payload;
pub mod procs;
pub mod receiver;
pub mod shell;
pub mod spool;
pub mod statusline;
#[cfg(test)]
mod testutil;
