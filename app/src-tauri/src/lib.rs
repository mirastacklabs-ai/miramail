// iPhone build: the Mac-only code paths (window geometry, tray, launch at login) are compiled out or stubbed, so some helpers are unused there.
#![cfg_attr(mobile, allow(dead_code, unused_imports, unused_variables))]

mod ai;
mod automation;
mod backup;
mod bgsync;
mod cache_ctl;
mod cal_cmds;
mod cal_invite;
mod cal_logic;
mod cal_sched;
mod cal_state;
mod commands;
mod composer;
mod contacts;
mod draft_mirror;
mod drafts;
mod files;
mod secrets_cache;
mod live;
mod localorg;
mod mbox_export;
mod snooze;
mod organise;
mod safety_cmds;
mod outbox;
mod panes;
mod prefetch;
mod presence;
mod state;
mod textsafe;
mod vcard;
mod vacation;
mod wire;

mod badge;
mod bgcheck;
mod mailto;
mod notify;
mod plat_prefs;
#[cfg(desktop)]
mod menu;
#[cfg(desktop)]
mod tray;
#[cfg(desktop)]
mod widget;
// iPhone: no menu bar, tray or widget window; no-op stand-ins keep the shared code and the UI's invokes working.
#[cfg(mobile)]
mod mobile;
#[cfg(mobile)]
use mobile::{menu, tray, widget};
mod native;
mod linknav;
mod winstate;

use mailcore::{KeychainSecretStore, SecretStore, Storage};
use state::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Manager;

static SERVICES_STARTED: AtomicBool = AtomicBool::new(false);
static WATCHERS_RUNNING: AtomicBool = AtomicBool::new(false);
/// Bumped by every watcher start and every stop; a start task whose generation is stale quits (see `start_watchers`).
static WATCH_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Presence changes are applied one at a time (Swift reports on the main thread; setup applies the first one).
static PRESENCE_APPLY: Mutex<()> = Mutex::new(());

/// Run one service start, so a failing start is logged and never keeps the others from starting. (Release builds use
/// `panic = "abort"`, where a panic ends the process anyway; this matters for debug builds.)
fn start_one(name: &str, f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        eprintln!("services: {name} failed to start");
    }
}

/// The long-running services of the open app, at most once per process: the first time the user is present (at once on
/// a Mac). On iPhone each of them also holds back while the user is away (`presence::user_present`).
fn start_services(app: &tauri::AppHandle) {
    if SERVICES_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let storage = app.state::<AppState>().storage.clone();
    start_one("backfill", || organise::start_backfill(storage));
    start_one("outbox", || outbox::start(app));
    // Snooze: overdue entries return shortly after start (app must be open).
    start_one("snooze", || snooze::start(app));
    start_one("draft mirror sweep", || draft_mirror::start_sweep(app));
    start_one("calendar", || cal_sched::start(app));
    start_one("badge", || badge::start(app));
}

/// Start an INBOX watcher (+ folder sync loop + live folders) for every saved account (Keychain read off the calling
/// thread), then either the launch pass (first time: sync the Inbox, run rules / auto-reply over what arrived while
/// MiraMail was closed) or the catch-up pass (back from the background: rules / auto-reply over what arrived meanwhile).
fn start_watchers(app: &tauri::AppHandle, launch_pass: bool) {
    // Generation: a stop (or a newer start) invalidates this task, so a slow Keychain read can never start a watcher
    // (and its live-folder restore) after the user left or alongside a newer start.
    let gen = WATCH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let storage = app.state::<AppState>().storage.clone();
    let secrets = app.state::<AppState>().secrets.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let accounts = storage.list_accounts().unwrap_or_default();
        let emails: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
        for cfg in accounts {
            // The user left while the Keychain was being read: the next arrival starts them again.
            if !presence::user_present() || WATCH_GEN.load(Ordering::SeqCst) != gen {
                return;
            }
            let s = secrets.clone();
            let email = cfg.email.clone();
            let pw = tauri::async_runtime::spawn_blocking(move || s.get(&email)).await;
            if !presence::user_present() || WATCH_GEN.load(Ordering::SeqCst) != gen {
                return;
            }
            if let Ok(Ok(Some(pw))) = pw {
                // Once on start / when the user is back: fetch the newest uncached INBOX bodies a moment after the first sync.
                prefetch::kick(&handle, &cfg.email, state::INBOX, std::time::Duration::from_secs(8));
                state::start_watcher(&handle, cfg, pw);
            }
        }
        if launch_pass {
            automation::start_launch_pass(&handle, emails);
        } else {
            automation::start_catch_up_pass(&handle, emails);
        }
    });
}

/// The user arrived (`present`) or left. On a Mac this runs once, from setup, with `present = true`, and does exactly
/// what setup always did. On iPhone it runs at setup and on every change: everything that talks to the mail servers or
/// changes mail on its own starts only once the user is in front of the app; leaving stops the watchers (the rest holds
/// back by itself). Idempotent; reads the live state rather than trusting the argument's order.
fn on_presence(app: &tauri::AppHandle) {
    let _g = PRESENCE_APPLY.lock().unwrap_or_else(|e| e.into_inner());
    let present = presence::user_present();
    let p = presence::plan(present, SERVICES_STARTED.load(Ordering::SeqCst), WATCHERS_RUNNING.load(Ordering::SeqCst));
    if p.start_services {
        start_services(app);
    }
    if p.start_watchers {
        WATCHERS_RUNNING.store(true, Ordering::SeqCst);
        start_watchers(app, p.launch_pass);
    }
    if p.stop_watchers {
        WATCHERS_RUNNING.store(false, Ordering::SeqCst);
        WATCH_GEN.fetch_add(1, Ordering::SeqCst);
        app.state::<AppState>().pause_watchers();
    }
    if !present {
        // The Reader's warm interactive session too: nothing stays logged in while the user is away (iPhone only).
        app.state::<AppState>().close_lanes();
    }
    #[cfg(target_os = "ios")]
    {
        use tauri::Emitter;
        // The page waits for this before anything that talks to a server (app/src/lib/foreground.ts).
        let _ = app.emit("app://presence", presence::launch_state_now());
        safety_cmds::perf::log(&format!("perf: presence present={present}"));
    }
}

/// iPhone: tell the Swift side what to run when iOS wakes the app. Reachable only from that wake-up: there is no command
/// for it, and nothing here reads launch arguments.
#[cfg(target_os = "ios")]
fn install_mailcheck(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri_plugin_mailcheck::set_check_handler(Box::new(move |token| {
        let t = token.clone();
        let cancelled: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || t.is_cancelled());
        // Hard cap on top of the per-account budget inside `run` (a stuck Keychain read or DNS lookup must not outlive it).
        let work = tauri::async_runtime::block_on(async {
            tokio::time::timeout(bgcheck::BUDGET + std::time::Duration::from_secs(3), bgcheck::run(&app, cancelled)).await
        });
        match work {
            Ok(Ok(s)) if s.cut || token.is_cancelled() => Ok(tauri_plugin_mailcheck::Outcome::Cut(s.text())),
            Ok(Ok(s)) if s.failed > 0 && s.failed == s.accounts => Err(s.text()),
            Ok(Ok(s)) => Ok(tauri_plugin_mailcheck::Outcome::Done(s.text())),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(tauri_plugin_mailcheck::Outcome::Cut("time budget exceeded".into())),
        }
    }));
}
#[cfg(not(target_os = "ios"))]
fn install_mailcheck(_app: &tauri::AppHandle) {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // iPhone only: background new-mail check (BGAppRefreshTask) + notification taps. No push service.
    #[cfg(target_os = "ios")]
    let builder = builder.plugin(tauri_plugin_mailcheck::init());
    builder
        .plugin(linknav::plugin())
        .on_window_event(winstate::on_window_event)
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            safety_cmds::perf::init(&dir);
            let storage = Arc::new(Storage::open(dir.join("miramail.db"))?);
            let secrets: Arc<dyn SecretStore> = Arc::new(secrets_cache::CachedSecretStore::new(KeychainSecretStore));
            let state = AppState::new(storage.clone(), secrets.clone());
            state.init_config(dir.join(bgsync::SETTINGS_FILE));
            app.manage(state);
            // Offline-cache limits (Preferences > Privacy > Storage): loaded before any sync can cache a body.
            cache_ctl::load_into(&storage, &dir.join(cache_ctl::FILE));
            app.manage(cache_ctl::PolicyFile::new(dir.join(cache_ctl::FILE)));
            app.manage(mbox_export::MboxState::default());
            // Workspace: drafts + pane layout in their own database file.
            app.manage(drafts::DraftStore::open(dir.join("workspace.db"))?);
            // Detached panes (handoff + geometry) in the same file; reopen the windows that were open at the last quit.
            app.manage(panes::PaneStore::open(dir.join("workspace.db"))?);
            // Signatures, From identities (same workspace database, own tables).
            app.manage(composer::ComposeStore::open(dir.join("workspace.db"))?);
            // Send pipeline: undo hold + persistent outbox, and the IMAP Drafts mirror (same workspace database). Opening the
            // outbox turns anything left `sending` into `unknown`: nothing is ever re-sent by itself after a launch.
            app.manage(outbox::Outbox::open(dir.join("workspace.db"))?);
            app.manage(draft_mirror::DraftMirror::open(dir.join("workspace.db"))?);
            // Address book (same workspace database, own tables). Local only.
            app.manage(contacts::ContactStore::open(dir.join("workspace.db"))?);
            // Threads, mute / pin / VIP / blocked senders: local tables in the same workspace database. No network.
            app.manage(localorg::LocalOrg::open(dir.join("workspace.db"))?);
            // Snooze: entries in the same workspace database; overdue ones return shortly after launch (app must be open).
            app.manage(snooze::SnoozeState { store: snooze::SnoozeStore::open(dir.join("workspace.db"))? });
            app.manage(panes::Shutdown::default());
            // Calendar: own database file; CalDAV passwords + Zoom tokens in the same (cached) Keychain store.
            let cal = calendar::Calendar::open(dir.join("calendar.db")).map_err(|e| format!("couldn't open the calendar database: {e}"))?;
            app.manage(cal_state::CalState::new(cal, secrets.clone()));
            // AI chat: own database file (ai.db), keys in the same cached Keychain store. Nothing is contacted here.
            let ai = ai::AiState::open(&dir.join("ai.db"), secrets.clone())?;
            ai.startup_purge();
            // Read-only mail access for AI tools (used only by conversations that enabled tools).
            let ai_handle = app.handle().clone();
            let connect: ai::host::ConnectFn = Arc::new(move |email: String| {
                let h = ai_handle.clone();
                Box::pin(async move { h.state::<AppState>().connect(&email).await })
            });
            let mail_host = Arc::new(ai::host::AppMailHost::new(storage.clone(), Some(connect)));
            ai.set_mail_host(mail_host.clone());
            // Read-only: lets draft_reply / draft_new look up the original message and known addresses.
            ai.set_draft_host(mail_host.clone());
            // Used only by the proposal executor after the user approves (or undoes) a suggestion.
            ai.set_action_host(mail_host.clone());
            // Calendar tools: read access for conversations that ticked the calendar; the write half is used only by the
            // calendar executor after the user approves (or undoes) a card.
            let cal_env = Arc::new(ai::calhost::AppCalEnv { app: app.handle().clone() });
            let cal_host = Arc::new(ai::calhost::CalAppHost::new(cal_env.clone(), cal_state::RealCalDav, ai::calhost::EnvZoom(cal_env)));
            ai.set_calendar_hosts(cal_host.clone(), cal_host);
            // Local rules + template auto-reply: same ai.db and executor, no model, no AI endpoint.
            let cache = Arc::new(automation::CacheMail::new(storage.clone()));
            ai.set_rule_hook(Arc::new(automation::AppRuleHook { store: ai.store_handle(), busy: ai.busy_handle(), actions: mail_host.clone(), mail: cache.clone(), now: Arc::new(automation::system_now) }));
            let auto = automation::Automation::new(
                ai.store_handle(),
                ai.busy_handle(),
                mail_host,
                cache.clone(),
                cache,
                Arc::new(automation::AppSender { app: app.handle().clone() }),
                Arc::new(automation::system_now),
            );
            app.manage(auto);
            app.manage(ai);
            // Platform integration: prefs, native menu bar, Dock badge / menu-bar item / widget / notifications.
            let plat = plat_prefs::PlatformState::default();
            plat.init(dir.join(plat_prefs::FILE));
            app.manage(plat);
            app.manage(badge::BadgeState::default());
            app.manage(mailto::Pending::default());
            // The main window is built here, not from tauri.conf.json (`"create": false`), so it can get the same new-window /
            // download hardening as the others (see linknav::harden). All state is managed by now, so the first IPC call
            // from the page finds it; everything below that looks up "main" runs after this.
            let main_cfg = app.config().app.windows.first().ok_or("no main window in tauri.conf.json")?.clone();
            linknav::harden(tauri::WebviewWindowBuilder::from_config(app.handle(), &main_cfg)?, app.handle(), &main_cfg.label).build()?;
            #[cfg(desktop)]
            {
                menu::install(app)?;
                winstate::restore_main(app.handle());
            }
            widget::sync(app.handle());
            install_mailcheck(app.handle());
            #[cfg(desktop)]
            panes::restore(app.handle());

            // Everything that talks to the mail servers or changes mail on its own (watchers, rules / auto-reply, scheduled
            // send, snooze, Junk moves, CalDAV, ...) starts once the user is in front of the app: here on a Mac; on iPhone
            // here or on the first didBecomeActive (a background launch only reads and stores new mail, bgcheck.rs), and
            // it is held back again whenever the user leaves (presence.rs).
            #[cfg(target_os = "ios")]
            {
                let h = app.handle().clone();
                // Called once now with the current state (covering any report Swift made before this), then on every change.
                tauri_plugin_mailcheck::set_presence_handler(Arc::new(move |_| on_presence(&h)));
            }
            #[cfg(not(target_os = "ios"))]
            on_presence(app.handle());
            Ok(())
        })
        .invoke_handler(widget::guard(tauri::generate_handler![
            commands::add_account,
            commands::list_accounts,
            commands::remove_account,
            commands::list_folders,
            commands::unread_counts,
            commands::sync_folder,
            commands::list_messages,
            commands::load_older,
            commands::get_message,
            commands::get_attachment,
            commands::message_analysis,
            safety_cmds::safety_report,
            safety_cmds::perf_log,
            commands::set_flag,
            commands::move_message,
            commands::move_message_to,
            commands::delete_message,
            commands::save_attachment_to_downloads,
            commands::get_sync_settings,
            commands::set_sync_settings,
            commands::list_live_folders,
            commands::live_states,
            commands::set_folder_live,
            organise::search_mail,
            organise::search_server,
            organise::folder_create,
            organise::folder_rename,
            organise::folder_move,
            organise::folder_message_count,
            organise::folder_delete,
            organise::folder_subscribe,
            organise::folder_subscriptions,
            organise::folder_empty,
            organise::bulk_move,
            organise::bulk_flag,
            organise::bulk_delete_forever,
            organise::sender_images_verified,
            organise::set_colour,
            organise::list_colours,
            organise::unsubscribe_info,
            organise::unsubscribe_mailto,
            vacation::vacation_status,
            vacation::vacation_enable,
            vacation::vacation_disable,
            organise::unsubscribe_one_click,
            organise::get_source,
            organise::save_eml,
            organise::open_attachment,
            organise::print_page,
            composer::composer_data,
            composer::save_signature,
            composer::delete_signature,
            composer::set_signature_defaults,
            composer::save_identity,
            composer::delete_identity,
            composer::sanitize_compose,
            composer::suggest_addresses,
            contacts::contacts_list,
            contacts::contacts_save,
            contacts::contacts_delete,
            contacts::contacts_groups,
            contacts::contacts_group_rename,
            contacts::contacts_group_delete,
            contacts::contacts_suggest,
            contacts::contacts_expand_group,
            contacts::contacts_card,
            contacts::contacts_from_mail,
            contacts::contacts_add_many,
            contacts::contacts_import,
            contacts::contacts_export,
            composer::get_raw_message,
            drafts::save_draft,
            drafts::list_drafts,
            drafts::delete_draft,
            drafts::save_layout,
            drafts::load_layout,
            outbox::outbox_submit,
            outbox::outbox_list,
            outbox::outbox_undo,
            outbox::outbox_confirm_edit,
            outbox::outbox_retry,
            outbox::outbox_discard,
            snooze::snooze_info,
            snooze::snooze_add,
            snooze::snooze_list,
            snooze::snooze_cancel,
            snooze::snooze_forget,
            snooze::snooze_count,
            mbox_export::mbox_export,
            mbox_export::mbox_cancel,
            backup::backup_export,
            backup::backup_inspect,
            cache_ctl::storage_usage,
            cache_ctl::storage_clear_bodies,
            cache_ctl::storage_set_policy,
            draft_mirror::draft_mirror_save,
            draft_mirror::draft_mirror_remove,
            panes::detach_pane,
            panes::get_pane,
            panes::save_pane,
            panes::attach_pane,
            panes::release_pane,
            panes::close_pane_window,
            panes::list_windows,
            panes::window_at,
            panes::focus_window,
            cal_cmds::cal_list_calendars,
            cal_cmds::cal_create_local,
            cal_cmds::cal_add_caldav,
            cal_cmds::cal_suggest_caldav,
            cal_cmds::cal_reusable_accounts,
            cal_cmds::cal_update_calendar,
            cal_cmds::cal_remove_calendar,
            cal_cmds::cal_sync,
            cal_cmds::cal_list_events,
            cal_cmds::cal_get_event,
            cal_cmds::cal_save_event,
            cal_cmds::cal_delete_event,
            cal_cmds::cal_respond_invite,
            cal_cmds::cal_import_invite,
            cal_cmds::zoom_status,
            cal_cmds::zoom_set_client,
            cal_cmds::zoom_connect,
            cal_cmds::zoom_disconnect,
            cal_cmds::zoom_create_meeting,
            cal_cmds::zoom_delete_meeting,
            ai::commands::ai_list_profiles,
            ai::commands::ai_save_profile,
            ai::commands::ai_delete_profile,
            ai::commands::ai_test_profile,
            ai::commands::ai_list_models,
            ai::commands::ai_check_tone,
            ai::commands::ai_get_settings,
            ai::commands::ai_set_settings,
            ai::commands::ai_new_conversation,
            ai::commands::ai_update_consent,
            ai::commands::ai_list_conversations,
            ai::commands::ai_get_conversation,
            ai::commands::ai_delete_conversation,
            ai::commands::ai_send,
            ai::commands::ai_cancel,
            ai::commands::ai_egress,
            ai::commands::ai_purge_now,
            ai::commands::ai_list_proposals,
            ai::commands::ai_get_proposal,
            ai::commands::ai_approve_proposal,
            ai::commands::ai_reject_proposal,
            ai::commands::ai_undo_proposal,
            ai::commands::ai_list_drafts,
            ai::commands::ai_get_draft,
            ai::commands::ai_open_draft,
            ai::commands::ai_dismiss_draft,
            automation::ai_rules_list,
            automation::ai_rule_save,
            automation::ai_rule_set_enabled,
            automation::ai_rule_delete,
            automation::ai_rule_card,
            automation::ai_rule_dry_run,
            automation::ai_rule_preview_existing,
            automation::ai_rule_apply_existing,
            automation::ai_automation_state,
            automation::ai_rules_pause,
            automation::ai_autoreply_pause,
            automation::ai_rule_runs,
            automation::ai_rule_run_undo,
            automation::ai_autoreply_get,
            automation::ai_autoreply_set,
            automation::ai_autoreply_preview,
            automation::ai_autoreply_log,
            badge::get_inbox_summary,
            badge::get_platform_prefs,
            badge::set_platform_prefs,
            widget::focus_main,
            widget::widget_menu,
            mailto::take_pending_mailto,
            notify::notify_test,
            notify::notification_tap,
            localorg::thread_map,
            localorg::thread_members,
            localorg::mute_thread,
            localorg::local_marks,
            localorg::pin_messages,
            localorg::sender_lists,
            localorg::set_vip,
            localorg::set_blocked,
            menu::get_launch_at_login,
            menu::set_launch_at_login,
            native::platform_info,
            native::open_link,
            presence::launch_state,
        ]))
        .build(tauri::generate_context!())
        .expect("error while building MiraMail")
        .run(|app, event| {
            // Cmd+Q / last window closed: persist every window first, then exit (main goes last).
            match event {
                tauri::RunEvent::ExitRequested { api, .. } => {
                    winstate::save_all(app);
                    if !panes::shutting_down(app) {
                        api.prevent_exit();
                        panes::begin_shutdown(app);
                    }
                }
                // A mailto: link opened MiraMail (macOS delivers it as an "open URL" event).
                tauri::RunEvent::Opened { urls } => mailto::on_opened(app, &urls),
                // Dock icon clicked with no window showing.
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen { has_visible_windows: false, .. } => menu::show_main(app),
                _ => {}
            }
        });
}
