//! Phase 1 console runner: draws the mockup as text.
//!
//! Useful to see the core working without a UI, and to diagnose when the
//! widget shows something odd. Its output is a developer tool, so it is in
//! English; limit labels come from the API normalization and are shown as-is.
//!
//!   cargo run -p cc-usage --example probe              one pass
//!   cargo run -p cc-usage --example probe -- --watch   refreshes every 60 s
//!   cargo run -p cc-usage --example probe -- --watch 10

use cc_usage::{api, model::Limit, refresh, statusline::LiveSnapshot, Snapshot};
use chrono::{DateTime, Utc};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let watch = args.iter().any(|a| a == "--watch");
    let every = args
        .iter()
        .skip_while(|a| *a != "--watch")
        .nth(1)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);

    let client = match api::Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not create the HTTP client: {e}");
            std::process::exit(1);
        }
    };

    // The profile is fetched only once and reused on every pass.
    let profile = {
        let s = refresh(&client, None).await;
        render(&s);
        s.email
    };

    if !watch {
        return;
    }

    let cached = profile.map(|email| api::Profile {
        account: Some(api::Account {
            email: Some(email),
            full_name: None,
        }),
        organization: None,
    });

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(every)).await;
        print!("\x1b[2J\x1b[H"); // clear the screen
        render(&refresh(&client, cached.as_ref()).await);
    }
}

fn render(s: &Snapshot) {
    let width = 46;
    println!("┌{}┐", "─".repeat(width));
    let title = "Claude Usage";
    let email = s.email.clone().unwrap_or_default();
    let plan = s.plan_label.clone().unwrap_or_default();
    let right = if email.is_empty() {
        plan.clone()
    } else {
        email
    };
    println!(
        "│ {title}{}{right} │",
        " ".repeat(padding(width, cols(title) + cols(&right) + 2))
    );
    if !plan.is_empty() && !right.eq(&plan) {
        println!("│ {plan}{} │", " ".repeat(padding(width, cols(&plan) + 2)));
    }
    println!("├{}┤", "─".repeat(width));

    match (&s.usage, &s.usage_error) {
        (Some(u), _) => {
            for limit in &u.limits {
                limit_block(width, limit);
            }
            if let Some(sp) = &u.spend {
                if sp.enabled {
                    let line = format!("Credits: {:.2} {}", sp.used, sp.currency);
                    println!("│ {line}{} │", " ".repeat(padding(width, cols(&line) + 2)));
                }
            }
        }
        (None, Some(e)) => {
            println!("│ ⚠ {e:<w$} │", w = width.saturating_sub(4));
        }
        (None, None) => {}
    }

    // Context window: only if it is fresh. A number from a finished
    // session, shown as current, is worse than showing nothing.
    if let Some(live) = &s.live {
        context_block(width, live);
    }

    println!("├{}┤", "─".repeat(width));
    let footer = format!(
        "Updated {}{}",
        clock(s.at),
        if s.is_stale() { " · STALE DATA" } else { "" }
    );
    println!(
        "│ {footer}{} │",
        " ".repeat(padding(width, cols(&footer) + 2))
    );
    println!("└{}┘", "─".repeat(width));

    if s.live.is_none() {
        println!("\n(no context window: run `cc-hook install` to chain the status line, then open Claude Code)");
    }
}

fn limit_block(width: usize, l: &Limit) {
    let pct = format!("{}%", l.percent.round() as i64);
    let active = if l.is_active { " ●" } else { "" };
    let left = format!("{}{active}", l.label);
    let gap = padding(width, cols(&left) + cols(&pct) + 2);
    println!("│ {left}{}{pct} │", " ".repeat(gap));
    println!("│ {} │", bar(l.percent, width - 2));
    let reset = l
        .resets_at
        .map(|t| format!("  resets in {}", time_until(t)))
        .unwrap_or_else(|| "  no reset time".into());
    println!("│ {reset:<w$} │", w = width.saturating_sub(2));
}

fn context_block(width: usize, live: &LiveSnapshot) {
    let Some(ctx) = &live.context else { return };
    if !live.is_fresh() {
        let notice = format!(
            "Context window: no active session ({} ago)",
            span(live.age())
        );
        println!("│ {notice:<w$} │", w = width.saturating_sub(2));
        return;
    }
    let pct = format!("{}%", ctx.percent.round() as i64);
    let left = "Context Window";
    println!(
        "│ {left}{}{pct} │",
        " ".repeat(padding(width, cols(left) + cols(&pct) + 2))
    );
    println!("│ {} │", bar(ctx.percent, width - 2));
    let detail = match (ctx.tokens_label(), &ctx.model) {
        (Some(t), Some(m)) => format!("  {t} · {m}"),
        (Some(t), None) => format!("  {t}"),
        (None, Some(m)) => format!("  {m}"),
        (None, None) => String::new(),
    };
    println!("│ {detail:<w$} │", w = width.saturating_sub(2));
}

fn bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn padding(width: usize, used: usize) -> usize {
    width.saturating_sub(used)
}

/// Width in characters, not bytes. `"5-hour session ●".len()` gives 18 (UTF-8
/// bytes) against 16 real columns, and the difference misaligns the box.
fn cols(s: &str) -> usize {
    s.chars().count()
}

fn clock(t: DateTime<Utc>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%H:%M:%S")
        .to_string()
}

fn time_until(t: DateTime<Utc>) -> String {
    let d = t - Utc::now();
    if d.num_seconds() <= 0 {
        return "now".into();
    }
    span(d)
}

fn span(d: chrono::Duration) -> String {
    let (days, hours, mins) = (d.num_days(), d.num_hours() % 24, d.num_minutes() % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}
