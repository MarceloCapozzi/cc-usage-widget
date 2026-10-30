// No console in release: it is a tray app, a black window behind it would ruin
// the effect. In debug it is kept so the logs are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tray app + floating widget.
//!
//! This binary is the only part that knows about Tauri. All the logic lives
//! in the library (`lib.rs` and its modules), which is tested without
//! starting WebView2. This file builds the app and starts its loops; the
//! binary's modules apply the library to Tauri:
//!
//! - `state`: [`AppState`], [`Widget`] and [`TrayItems`], the state Tauri
//!   manages, and `lock` / `config_dir`.
//! - `commands`: the commands the frontend invokes.
//! - `poller`: the usage poll, the session pass, the Publisher start and
//!   the session rows sent to the frontend.
//! - `tray`: the tray icon, its menu, styles, badge and tooltip.
//! - `window`: the widget window's geometry, movement, fitted height,
//!   reset, full-screen hiding and shortcut.
//! - `devflags`: the development flags (debug builds only).
//! - `links`: opening an `https://` link in the browser.
//!
//! The widget's window rules: always on top, no
//! frame, moved by dragging the header or any empty part of the panel (also
//! across monitors with different scaling), resized from invisible grips on
//! every edge and corner, its position and size remembered in `window.json`
//! in the app config dir, and shown or hidden with a user-defined global
//! shortcut (`settings.json`, edited in the ⚙ view). The geometry rules and
//! the settings are in the library (`window_state`, `settings`,
//! `fullscreen`); `window` applies them to the window.
//!
//! User-facing text (tray menu, tooltips, toasts) is in English; the
//! Rust ↔ JS contract (command names, event names, argument names) must match
//! the modules in `ui/scripts/`.

mod commands;
mod devflags;
mod links;
mod poller;
mod state;
mod tray;
mod window;

use crate::commands::Inspected;
use crate::devflags::{dev_flags, show_reference};
use crate::poller::{
    poll, poll_sessions, poll_wake, remote_changed, start_publisher, wait_for_renewal,
};
use crate::state::{config_dir, lock, AppState, TrayItems, Widget};
use crate::tray::{build_tray, TOOLTIP_LOADING};
use crate::window::{
    mark_moved, note_resize, register_shortcut, restore, save, show_widget, watch_fullscreen,
    watch_movement, WINDOW,
};
use cc_usage::{
    api, history, instance, notifications, publisher, remote, sessions as local,
    settings::{self, Role},
};
use std::sync::{Arc, OnceLock};
use tauri::{Manager, WindowEvent};
use tokio::sync::Mutex;

fn main() {
    // One app per data folder, before anything reads or writes its files: a
    // second copy (the Start menu while the Run key's copy runs) shows the
    // first one's widget and ends here.
    let key = instance::key(&cc_usage::paths::data_dir());
    let held = match instance::claim(&key) {
        instance::Start::First(held) => held,
        instance::Start::Running => {
            instance::ask_to_show(&key);
            return;
        }
    };
    let show_requests = held.show_requests();
    instance::hold(held);

    // Both roles log to `cc-usage.log` in the data folder (and to stderr,
    // which only a debug build has): release builds have no console.
    cc_publish::logging::init(Some(&cc_usage::paths::file(cc_usage::paths::LOG)));

    let client = match api::Client::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("could not create the HTTP client: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(AppState {
        client,
        profile: Mutex::new(None),
        usage_report: Mutex::new(Default::default()),
        notif_state: Mutex::new(notifications::State::load()),
        history: Mutex::new(history::History::load()),
        last: Mutex::new(None),
        sessions: std::sync::Mutex::new(local::SessionMonitor::new()),
        local_rows: std::sync::Mutex::new(None),
        remote: remote::RemoteHub::shared(),
        usage_tooltip: std::sync::Mutex::new(TOOLTIP_LOADING.into()),
        idle_filter: std::sync::Mutex::new(local::IdleFilter::default()),
        sent_rows: std::sync::Mutex::new(None),
        machine_name: std::sync::Mutex::new(settings::default_machine_name()),
        role: OnceLock::new(),
        publisher: OnceLock::new(),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .manage(state.clone())
        .manage(Widget::default())
        .manage(TrayItems::default())
        .manage(Inspected::default())
        .invoke_handler(tauri::generate_handler![
            commands::refresh_now,
            commands::set_start_with_windows,
            commands::last_snapshot,
            commands::open_usage,
            commands::usage_by_project,
            commands::history,
            commands::close_flyout,
            commands::sessions,
            commands::remote_status,
            commands::remote_pending_keys,
            commands::accept_publisher_key,
            commands::reject_publisher_key,
            commands::focus_session,
            commands::focus_preview,
            commands::ui_flags,
            commands::fit_height,
            commands::start_resize,
            commands::reset_window,
            commands::get_settings,
            commands::set_settings,
            commands::pause_shortcut,
            commands::clear_sessions,
            commands::backup_data,
            commands::inspect_backup,
            commands::restore_backup,
            commands::set_role,
            commands::restart_app,
            commands::publisher_generate_key,
            commands::publisher_import_viewer_key,
            commands::publisher_import_creds
        ])
        .setup(move |app| {
            build_tray(app.handle())?;

            let handle = app.handle().clone();
            let prefs = settings::read(&config_dir(&handle));
            // The role is fixed for this run (switching it restarts).
            let _ = state.role.set(prefs.role);
            if prefs.role == Role::Publisher {
                let alias = prefs.machine_name_or(&settings::default_machine_name());
                let hub = publisher::PublisherHub::new(publisher::Setup::from_data_dir(&alias));
                let _ = state.publisher.set(Arc::new(hub));
            } else {
                // Other machines' projects as last heard of, and the
                // publisher keys waiting for review, kept between runs. A
                // Publisher shows this machine only.
                let mut hub = state.remote.lock().unwrap_or_else(|e| e.into_inner());
                hub.keep_projects_in(
                    cc_core::paths::data_dir().join(remote::REMOTE_PROJECTS_FILE),
                    remote::now_unix(),
                );
                hub.keep_pending_in(cc_core::paths::publishers_dir());
            }
            state.apply_row_settings(&prefs);
            // Recorded, not shown here: the ⚙ view reports it next to the
            // field whenever it is opened, so a shortcut that failed at
            // startup still explains itself.
            let problem = register_shortcut(&handle, &prefs).err();
            {
                let widget = handle.state::<Widget>();
                *lock(&widget.shortcut_error) = problem;
                *lock(&widget.settings) = prefs;
            }

            // Position and size first, then the window is shown: showing it
            // first would flash it at the conf's default spot before it jumped.
            if let Some(w) = app.get_webview_window(WINDOW) {
                restore(&w);
            }
            watch_movement(handle.clone());
            watch_fullscreen(handle.clone());
            // A second copy started meanwhile asks for the widget.
            if let Some(requests) = show_requests {
                std::thread::spawn(move || {
                    while requests.wait(None) {
                        show_widget(&handle);
                    }
                });
            }

            if let Some(page) = &dev_flags().reference {
                show_reference(app.handle(), page);
                return Ok(());
            }

            let handle = app.handle().clone();
            let st = state.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let expired = poll(&handle, &st).await;
                    // The interval is read every turn ("Refresh usage every"
                    // in the ⚙ view); a change wakes the wait and refreshes.
                    // With an expired token the wait also ends when Claude
                    // Code renews it.
                    let every = lock(&handle.state::<Widget>().settings).refresh_interval();
                    let wait = async {
                        if expired {
                            wait_for_renewal(every).await;
                        } else {
                            tokio::time::sleep(every).await;
                        }
                    };
                    tokio::select! {
                        _ = wait => {}
                        _ = poll_wake().notified() => {}
                    }
                }
            });

            // Demo mode never reads the real session sources: the `sessions`
            // command answers with the fixture, and nothing toasts. The
            // fixture is built on that first call, when the page loads, so
            // its clocks start at the mockup's values when it first paints.
            if dev_flags().demo {
                return Ok(());
            }

            // Local sessions, on their own loop: they change by the second,
            // usage by the minute. The usage poll above is untouched.
            let handle = app.handle().clone();
            let st = state.clone();
            tauri::async_runtime::spawn(async move {
                let mut sources = local::sources();
                loop {
                    sources = poll_sessions(&handle, &st, sources).await;
                    tokio::time::sleep(local::POLL_INTERVAL).await;
                }
            });

            // Publisher role: no viewer connection. This machine's sessions
            // go out through the session pass above; the network side starts
            // as soon as the creds and both keys are there (looked at again
            // every 30 s, and at once after an import in Settings).
            if state.is_publisher() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        let h = handle.clone();
                        let started =
                            tauri::async_runtime::spawn_blocking(move || start_publisher(&h))
                                .await
                                .unwrap_or(false);
                        if started {
                            break;
                        }
                        tokio::time::sleep(remote::SETUP_RECHECK).await;
                    }
                });
                return Ok(());
            }

            // Remote sessions: one viewer connection to the KV bucket, when
            // this machine has the viewer creds (its key pair is created on
            // first start). It reports "not configured" otherwise and looks
            // again later.
            let handle = app.handle().clone();
            let st = state.clone();
            let notify: remote::Notify = Arc::new(move || remote_changed(&handle, &st));
            let hub = state.remote.clone();
            tauri::async_runtime::spawn(remote::run(
                hub.clone(),
                remote::Setup::from_data_dir(),
                notify.clone(),
            ));
            tauri::async_runtime::spawn(remote::run_ticker(hub, notify));
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != WINDOW {
                return;
            }
            let app = window.app_handle();
            match event {
                WindowEvent::Moved(_) => mark_moved(app),
                WindowEvent::Resized(size) => {
                    if let Some(w) = app.get_webview_window(WINDOW) {
                        note_resize(&w, *size);
                    }
                    mark_moved(app);
                }
                // Alt+F4 on the widget hides it instead of destroying the
                // only window: the tray and the shortcut bring it back.
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    if let Some(w) = app.get_webview_window(WINDOW) {
                        save(&w);
                        let _ = w.hide();
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("could not start the app")
        .run(|app, event| {
            // Closing the last window must not end the process: the app lives
            // in the tray and the widget comes and goes. That request carries
            // no exit code. `app.exit(code)` (the tray's Quit) carries
            // `Some(code)` and must go through: preventing every request
            // would make Quit do nothing.
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                api.prevent_exit();
            } else if let tauri::RunEvent::Exit = event {
                // Other machines' projects are written a few seconds after
                // they change, the seen times of the keys waiting for review
                // every few minutes; what is still pending goes now.
                if let Some(state) = app.try_state::<Arc<AppState>>() {
                    remote::flush_projects_of(&state.remote);
                    remote::flush_pending_of(&state.remote);
                }
            }
        });
}
