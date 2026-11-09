#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
mod state;
mod textsafe;
mod vcard;
mod vacation;
mod wire;

mod badge;
mod mailto;
mod menu;
mod notify;
mod plat_prefs;
mod tray;
mod widget;
mod native;
mod linknav;
mod winstate;

use mailcore::{KeychainSecretStore, SecretStore, Storage};
use state::AppState;
use std::sync::Arc;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(linknav::plugin())
        .on_window_event(winstate::on_window_event)
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let storage = Arc::new(Storage::open(dir.join("miramail.db"))?);
            let secrets: Arc<dyn SecretStore> = Arc::new(secrets_cache::CachedSecretStore::new(KeychainSecretStore));
            let state = AppState::new(storage.clone(), secrets.clone());
            state.init_config(dir.join(bgsync::SETTINGS_FILE));
            app.manage(state);
            organise::start_backfill(storage.clone());
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
            outbox::start(app.handle());
            // Snooze: entries in the same workspace database; overdue ones return shortly after launch (app must be open).
            app.manage(snooze::SnoozeState { store: snooze::SnoozeStore::open(dir.join("workspace.db"))? });
            snooze::start(app.handle());
            draft_mirror::start_sweep(app.handle());
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
            cal_sched::start(app.handle());
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
            menu::install(app)?;
            winstate::restore_main(app.handle());
            widget::sync(app.handle());
            badge::start(app.handle());
            panes::restore(app.handle());

            // Start an INBOX watcher for every saved account (Keychain read off the setup thread).
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let accounts = storage.list_accounts().unwrap_or_default();
                let emails: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
                for cfg in accounts {
                    let s = secrets.clone();
                    let email = cfg.email.clone();
                    let pw = tauri::async_runtime::spawn_blocking(move || s.get(&email)).await;
                    if let Ok(Ok(Some(pw))) = pw {
                        state::start_watcher(&handle, cfg, pw);
                    }
                }
                // Mail that arrived while MiraMail was closed: rules / auto-reply look at it once.
                automation::start_launch_pass(&handle, emails);
            });
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
            safety_cmds::safety_report,
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
                tauri::RunEvent::Reopen { has_visible_windows: false, .. } => menu::show_main(app),
                _ => {}
            }
        });
}
