//! Drains a spool directory through the state machine and prints what the
//! panel would show after each event.
//!
//! ```text
//! cargo run -p cc-local --example spool_view -- <spool dir>
//! ```
//!
//! Draining deletes the files it reads, so point it at a copy when the
//! originals matter. Each event is applied with `now` equal to its own
//! timestamp, as if the widget had seen it live; the last line is the list of
//! rows after every event, which is empty when the run ended with SessionEnd.

use std::path::PathBuf;

use cc_core::state::SessionTracker;

fn main() {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: spool_view <spool dir>");
        std::process::exit(2);
    };
    let events = cc_local::spool::drain(&dir);
    println!("drained {} event(s) from {}", events.len(), dir.display());

    let mut tracker = SessionTracker::new();
    for event in &events {
        tracker.apply_event(event, event.ts);
        tracker.tick(event.ts);
        println!("\nafter {} ({}):", event.event, event.ts);
        match tracker.session(&event.session_id) {
            Some(view) => println!(
                "{}",
                serde_json::to_string_pretty(&view).unwrap_or_default()
            ),
            None => println!("  (session removed)"),
        }
        for t in tracker.take_transitions() {
            println!("  transition {:?} -> {:?}", t.from, t.to);
        }
    }
    println!(
        "\nfinal rows: {}",
        serde_json::to_string(&tracker.sessions()).unwrap_or_default()
    );
}
