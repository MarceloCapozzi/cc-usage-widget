//! Entry point of the `cc-hook` binary. All logic lives in the `cc_hook`
//! library; see its crate documentation for the three modes (hook receiver,
//! settings installer and status line) and their silence contract.

#![warn(missing_docs)]

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let code = cc_hook::cli::run(&args);
    if code != 0 {
        std::process::exit(code);
    }
}
