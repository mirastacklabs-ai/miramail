use aichat::http::validate_base_url;
use aichat::store::*;

const T0: i64 = 1_800_000_000; // fixed fake clock
const DAY: i64 = 86_400;

fn store() -> Store {
    Store::open_in_memory().unwrap()
}
fn pin(id: &str, url: &str) -> ProfileInput {
    ProfileInput { id: id.into(), name: id.into(), dialect: "openai".into(), base_url: url.into(), model: "m".into(), extra_json: "{}".into() }
}
fn conv(accounts: &[&str]) -> NewConversation {
    NewConversation { title: "t".into(), profile_id: None, endpoint_host: "h".into(), model: "m".into(), consent_json: "{}".into(), accounts: accounts.iter().map(|s| s.to_string()).collect() }
}
fn egress(conv_id: Option<i64>, req: &str) -> NewEgress {
    NewEgress { round: 0, conv_id, profile_id: None, endpoint_host: "h".into(), model: "m".into(), request_json: req.into(), bytes: 10, est_tokens: 3, items_json: "[]".into(), redactions_json: "{}".into() }
}
fn count(s: &Store, table: &str) -> i64 {
    // id 0 is the reserved, hidden system conversation of the rule runner (schema v6)
    let filter = if table == "conversations" { " WHERE id > 0" } else { "" };
    s.raw().query_row(&format!("SELECT COUNT(*) FROM {table}{filter}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn fresh_db_is_current_version_with_tables() {
    let s = store();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    for t in ["provider_profiles", "settings", "scopes", "conversations", "chat_messages", "conv_refs", "egress", "usage_daily"] {
        assert_eq!(count(&s, t), 0, "{t}");
    }
}

#[test]
fn no_secret_columns_anywhere() {
    let s = store();
    let tables: Vec<String> = s
        .raw()
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, rusqlite::Error>>()
        .unwrap();
    assert!(tables.len() >= 8);
    for t in &tables {
        let cols: Vec<String> = s
            .raw()
            .prepare(&format!("PRAGMA table_info({t})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<_, rusqlite::Error>>()
            .unwrap();
        for c in cols {
            let c = c.to_lowercase();
            assert!(!(c.contains("secret") || c.contains("password") || c.contains("token") && c != "est_tokens" && c != "tokens_in" && c != "tokens_out" || c == "api_key" || c == "key" || c.contains("authorization")), "{t}.{c}");
        }
    }
}

#[test]
fn reopen_is_idempotent_and_newer_schema_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.db");
    {
        let mut s = Store::open(&p).unwrap();
        s.save_profile(&pin("a", "https://api.example.com/v1"), T0).unwrap();
    }
    {
        let s = Store::open(&p).unwrap();
        assert_eq!(s.list_profiles().unwrap().len(), 1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
    rusqlite::Connection::open(&p).unwrap().pragma_update(None, "user_version", 999).unwrap();
    match Store::open(&p) {
        Err(StoreError::NewerSchema { found: 999, .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn migrates_from_empty_v0_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.db");
    rusqlite::Connection::open(&p).unwrap(); // user_version 0, no tables
    let s = Store::open(&p).unwrap();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn v1_database_opens_and_keeps_data() {
    // Build a db at schema v1 using the fresh schema, then reopen: data must survive and version stays.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.db");
    {
        let mut s = Store::open(&p).unwrap();
        s.save_profile(&pin("a", "https://x.example/v1"), T0).unwrap();
        let c = s.create_conversation(&conv(&["acct"]), T0).unwrap();
        s.append_message(c, "user", "[]", T0).unwrap();
        // Simulate an older file: undo what the newer migrations (v3, v4, v5) added and force version 2.
        s.raw().execute_batch("DROP TABLE conv_folders; DROP TABLE rule_colour_log; DROP TABLE cal_log; DROP TABLE rule_moved; DROP TABLE autoreply_log; DROP TABLE autoreply_cursor; DROP TABLE autoreply_cfg; DROP TABLE rule_runs; DROP TABLE rule_cursor; DROP TABLE rules; DROP TABLE ai_drafts; DROP TABLE action_log; DROP TABLE proposal_items; DROP TABLE proposals; DROP TABLE tool_runs; ALTER TABLE egress DROP COLUMN round;").unwrap();
        s.raw().pragma_update(None, "user_version", 2).unwrap();
    }
    let s = Store::open(&p).unwrap();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(s.list_profiles().unwrap().len(), 1);
    assert_eq!(s.list_conversations().unwrap().len(), 1);
}

#[test]
fn v3_database_upgrades_to_proposals_and_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.db");
    {
        let mut s = Store::open(&p).unwrap();
        s.save_profile(&pin("a", "https://x.example/v1"), T0).unwrap();
        let c = s.create_conversation(&conv(&["acct"]), T0).unwrap();
        s.append_message(c, "user", "[]", T0).unwrap();
        s.raw().execute_batch("DROP TABLE conv_folders; DROP TABLE rule_colour_log; DROP TABLE cal_log; DROP TABLE rule_moved; DROP TABLE autoreply_log; DROP TABLE autoreply_cursor; DROP TABLE autoreply_cfg; DROP TABLE rule_runs; DROP TABLE rule_cursor; DROP TABLE rules; DROP TABLE ai_drafts; DROP TABLE action_log; DROP TABLE proposal_items; DROP TABLE proposals;").unwrap();
        s.raw().pragma_update(None, "user_version", 3).unwrap();
    }
    let mut s = Store::open(&p).unwrap();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(s.schema_version().unwrap(), 10);
    assert_eq!(s.list_conversations().unwrap().len(), 1);
    let n: i64 = s.raw().query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('proposals','proposal_items','action_log')", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 3);
    assert!(s.list_proposals(None, 10).unwrap().is_empty());
}

#[test]
fn v4_database_upgrades_to_drafts_and_cascades_with_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.db");
    let conv_id;
    {
        let mut s = Store::open(&p).unwrap();
        conv_id = s.create_conversation(&conv(&["acct"]), T0).unwrap();
        s.raw().execute_batch("DROP TABLE conv_folders; DROP TABLE rule_colour_log; DROP TABLE cal_log; DROP TABLE rule_moved; DROP TABLE autoreply_log; DROP TABLE autoreply_cursor; DROP TABLE autoreply_cfg; DROP TABLE rule_runs; DROP TABLE rule_cursor; DROP TABLE rules; DROP TABLE ai_drafts;").unwrap();
        s.raw().pragma_update(None, "user_version", 4).unwrap();
    }
    let mut s = Store::open(&p).unwrap();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(s.list_drafts(None, 10).unwrap().is_empty());
    s.raw().execute("PRAGMA foreign_keys=ON", []).ok();
    let planned = aichat::drafts::plan_new("acct", &["a@b.co".into()], &[], "s", "hello", &|_| true).unwrap();
    let id = s.create_draft(conv_id, &planned, "because", T0).unwrap();
    let d = s.get_draft(id).unwrap().unwrap();
    assert_eq!(d.state, aichat::drafts::DraftState::Proposed);
    // state machine: proposed -> opened (repeatable); dismissed only from proposed and final
    assert_eq!(s.mark_draft_opened(id, T0 + 1).unwrap().state, aichat::drafts::DraftState::Opened);
    assert_eq!(s.mark_draft_opened(id, T0 + 2).unwrap().state, aichat::drafts::DraftState::Opened);
    assert!(s.mark_draft_dismissed(id, T0 + 3).is_err(), "an opened draft cannot be dismissed");
    let id2 = s.create_draft(conv_id, &planned, "", T0).unwrap();
    assert_eq!(s.mark_draft_dismissed(id2, T0 + 1).unwrap().state, aichat::drafts::DraftState::Dismissed);
    assert!(s.mark_draft_opened(id2, T0 + 2).is_err(), "a dismissed draft cannot be opened");
    assert_eq!(s.mark_draft_opened(9999, T0), Err(aichat::drafts::DraftError::NotFound));
    s.delete_conversation(conv_id).unwrap();
    assert!(s.list_drafts(None, 10).unwrap().is_empty(), "drafts leave with their conversation");
}

#[test]
fn profile_crud_and_key_binding() {
    let mut s = store();
    let p = s.save_profile(&pin("a", "https://api.example.com/v1"), T0).unwrap();
    assert!(!p.has_key && p.key_host.is_none());
    assert!(!needs_key_reentry(&p));
    let bound = s.mark_key_saved("a", T0 + 1).unwrap();
    assert_eq!(bound, "https://api.example.com:443");
    let p = s.get_profile("a").unwrap().unwrap();
    assert!(p.has_key);
    assert!(!needs_key_reentry(&p));
    // path / model change keeps the key
    let mut inp = pin("a", "https://api.example.com/other");
    inp.model = "m2".into();
    let p = s.save_profile(&inp, T0 + 2).unwrap();
    assert!(p.has_key && !needs_key_reentry(&p));
    assert_eq!(p.created_at, T0);
    assert_eq!(p.updated_at, T0 + 2);
}

#[test]
fn changing_base_url_origin_forces_key_reentry() {
    for new in ["https://evil.example/v1", "http://api.example.com/v1", "https://api.example.com:8443/v1"] {
        let mut s = store();
        s.save_profile(&pin("a", "https://api.example.com/v1"), T0).unwrap();
        s.mark_key_saved("a", T0).unwrap();
        let p = s.save_profile(&pin("a", new), T0 + 1).unwrap();
        assert!(p.has_key, "{new}");
        assert!(needs_key_reentry(&p), "{new}");
        // re-entering binds to the new origin
        s.mark_key_saved("a", T0 + 2).unwrap();
        assert!(!needs_key_reentry(&s.get_profile("a").unwrap().unwrap()), "{new}");
    }
}

#[test]
fn key_reentry_when_url_unparseable_or_key_cleared() {
    let mut s = store();
    s.save_profile(&pin("a", "https://api.example.com/v1"), T0).unwrap();
    s.mark_key_saved("a", T0).unwrap();
    let p = s.save_profile(&pin("a", "not a url"), T0).unwrap();
    assert!(needs_key_reentry(&p));
    assert!(matches!(s.mark_key_saved("a", T0), Err(StoreError::InvalidProfile)));
    s.save_profile(&pin("a", "https://api.example.com/v1"), T0).unwrap();
    s.mark_key_cleared("a", T0).unwrap();
    let p = s.get_profile("a").unwrap().unwrap();
    assert!(!p.has_key && p.key_host.is_none() && !needs_key_reentry(&p));
    assert!(matches!(s.mark_key_cleared("zz", T0), Err(StoreError::NotFound)));
    assert!(matches!(s.mark_key_saved("zz", T0), Err(StoreError::NotFound)));
    // sanity for origin helper used by binding
    assert_eq!(validate_base_url("https://api.example.com/v1").unwrap().origin(), "https://api.example.com:443");
}

#[test]
fn delete_profile_keeps_conversations() {
    let mut s = store();
    s.save_profile(&pin("a", "http://localhost:11434/v1"), T0).unwrap();
    let mut c = conv(&[]);
    c.profile_id = Some("a".into());
    let id = s.create_conversation(&c, T0).unwrap();
    assert!(s.delete_profile("a").unwrap());
    assert!(!s.delete_profile("a").unwrap());
    assert!(s.get_conversation(id).unwrap().is_some());
}

#[test]
fn settings_and_purge_defaults() {
    let mut s = store();
    assert_eq!(s.get_setting("x").unwrap(), None);
    s.set_setting("x", "1").unwrap();
    s.set_setting("x", "2").unwrap();
    assert_eq!(s.get_setting("x").unwrap().as_deref(), Some("2"));
    let d = s.purge_settings().unwrap();
    assert_eq!(d, PurgeSettings::default());
    assert_eq!(d.egress_retention_days, Some(30));
    assert_eq!(d.conversation_retention_days, None);
    assert!(d.purge_on_account_removal);
    let n = PurgeSettings { egress_retention_days: None, conversation_retention_days: Some(7), purge_on_account_removal: false };
    s.set_purge_settings(&n).unwrap();
    assert_eq!(s.purge_settings().unwrap(), n);
    s.set_setting("purge", "garbage").unwrap();
    assert_eq!(s.purge_settings().unwrap(), PurgeSettings::default());
}

#[test]
fn scopes_roundtrip() {
    let mut s = store();
    s.set_scope("a@x", "INBOX", true).unwrap();
    s.set_scope("a@x", "INBOX", false).unwrap();
    s.set_scope("a@x", "Sent", true).unwrap();
    assert_eq!(s.list_scopes().unwrap(), vec![("a@x".into(), "INBOX".into(), false), ("a@x".into(), "Sent".into(), true)]);
}

#[test]
fn messages_sequence_and_order() {
    let mut s = store();
    let c = s.create_conversation(&conv(&[]), T0).unwrap();
    assert_eq!(s.append_message(c, "user", "[1]", T0 + 1).unwrap(), 1);
    assert_eq!(s.append_message(c, "assistant", "[2]", T0 + 2).unwrap(), 2);
    let ms = s.list_messages(c).unwrap();
    assert_eq!(ms.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(ms[1].parts_json, "[2]");
    assert_eq!(s.get_conversation(c).unwrap().unwrap().updated_at, T0 + 2);
    // FK enforced
    assert!(s.append_message(9999, "user", "[]", T0).is_err());
    assert_eq!(count(&s, "chat_messages"), 2);
}

#[test]
fn refs_upsert() {
    let mut s = store();
    let c = s.create_conversation(&conv(&["a"]), T0).unwrap();
    let r = ConvRef { reference: "m1".into(), account: "a".into(), folder: "INBOX".into(), uid: 5, uidvalidity: 7 };
    s.put_ref(c, &r).unwrap();
    s.put_ref(c, &ConvRef { uid: 6, ..r.clone() }).unwrap();
    assert_eq!(s.get_ref(c, "m1").unwrap().unwrap().uid, 6);
    assert!(s.get_ref(c, "zz").unwrap().is_none());
}

#[test]
fn egress_strips_auth_and_is_parameterised() {
    let mut s = store();
    let evil = "x'); DROP TABLE egress;--";
    let req = serde_json::json!({"model": evil, "Authorization": "Bearer sk-SECRET", "headers": {"x-api-key": "sk-SECRET"}, "api_key": "sk-SECRET", "messages": []}).to_string();
    let id = s.record_egress(&egress(None, &req), T0).unwrap();
    let rec = &s.list_egress(None, 10).unwrap()[0];
    assert_eq!(rec.id, id);
    assert!(!rec.request_json.contains("sk-SECRET"));
    assert!(!rec.request_json.to_lowercase().contains("authorization"));
    assert!(rec.request_json.contains("DROP TABLE"), "stored literally, not executed");
    assert_eq!((rec.bytes, rec.est_tokens), (10, 3));
    // non-JSON passes through
    s.record_egress(&egress(None, "plain"), T0 + 1).unwrap();
    assert_eq!(s.list_egress(None, 1).unwrap()[0].request_json, "plain");
}

#[test]
fn egress_filter_by_conversation() {
    let mut s = store();
    let a = s.create_conversation(&conv(&[]), T0).unwrap();
    let b = s.create_conversation(&conv(&[]), T0).unwrap();
    s.record_egress(&egress(Some(a), "{}"), T0).unwrap();
    s.record_egress(&egress(Some(b), "{}"), T0).unwrap();
    s.record_egress(&egress(Some(b), "{}"), T0 + 1).unwrap();
    assert_eq!(s.list_egress(Some(b), 10).unwrap().len(), 2);
    assert_eq!(s.list_egress(None, 10).unwrap().len(), 3);
    assert_eq!(s.list_egress(None, 2).unwrap().len(), 2);
}

#[test]
fn usage_aggregates_per_day() {
    let mut s = store();
    s.add_usage(T0, "p", "m", 10, 5).unwrap();
    s.add_usage(T0 + 5, "p", "m", 1, 2).unwrap();
    s.add_usage(T0 + DAY, "p", "m", 7, 7).unwrap();
    let u = s.list_usage("0000-00-00").unwrap();
    assert_eq!(u.len(), 2);
    assert_eq!((u[0].requests, u[0].tokens_in, u[0].tokens_out), (2, 11, 7));
    assert_eq!(u[1].day, day_string(T0 + DAY));
    assert_eq!(s.list_usage(&u[1].day).unwrap().len(), 1);
}

#[test]
fn day_string_known_values() {
    assert_eq!(day_string(0), "1970-01-01");
    assert_eq!(day_string(951_782_400), "2000-02-29");
    assert_eq!(day_string(1_800_000_000), "2027-01-15");
    assert_eq!(day_string(-1), "1969-12-31");
}

#[test]
fn purge_now_egress_with_fake_clock() {
    let mut s = store();
    s.record_egress(&egress(None, "{}"), T0 - 40 * DAY).unwrap();
    s.record_egress(&egress(None, "{}"), T0 - 31 * DAY).unwrap();
    s.record_egress(&egress(None, "{}"), T0 - 29 * DAY).unwrap();
    s.record_egress(&egress(None, "{}"), T0).unwrap();
    let rep = s.purge_now(&PurgeSettings::default(), T0).unwrap();
    assert_eq!(rep, PurgeReport { egress_deleted: 2, conversations_deleted: 0 });
    assert_eq!(count(&s, "egress"), 2);
    // None = keep forever
    let keep = PurgeSettings { egress_retention_days: None, ..PurgeSettings::default() };
    assert_eq!(s.purge_now(&keep, T0 + 1000 * DAY).unwrap(), PurgeReport::default());
    assert_eq!(count(&s, "egress"), 2);
    // Zero days: everything older than now
    let zero = PurgeSettings { egress_retention_days: Some(0), ..PurgeSettings::default() };
    assert_eq!(s.purge_now(&zero, T0 + 1).unwrap().egress_deleted, 2);
}

#[test]
fn purge_now_conversations_cascade() {
    let mut s = store();
    let old = s.create_conversation(&conv(&["a"]), T0 - 100 * DAY).unwrap();
    s.append_message(old, "user", "[]", T0 - 100 * DAY).unwrap();
    s.put_ref(old, &ConvRef { reference: "r".into(), account: "a".into(), folder: "f".into(), uid: 1, uidvalidity: 1 }).unwrap();
    s.record_egress(&egress(Some(old), "{}"), T0).unwrap();
    let fresh = s.create_conversation(&conv(&["a"]), T0 - DAY).unwrap();
    s.append_message(fresh, "user", "[]", T0 - DAY).unwrap();
    let p = PurgeSettings { egress_retention_days: None, conversation_retention_days: Some(30), purge_on_account_removal: true };
    let rep = s.purge_now(&p, T0).unwrap();
    assert_eq!(rep.conversations_deleted, 1);
    assert!(s.get_conversation(old).unwrap().is_none());
    assert_eq!(count(&s, "chat_messages"), 1);
    assert_eq!(count(&s, "conv_refs"), 0);
    assert_eq!(count(&s, "egress"), 0);
    assert!(s.get_conversation(fresh).unwrap().is_some());
}

#[test]
fn delete_conversation_cascades() {
    let mut s = store();
    let c = s.create_conversation(&conv(&[]), T0).unwrap();
    let other = s.create_conversation(&conv(&[]), T0).unwrap();
    for id in [c, other] {
        s.append_message(id, "user", "[]", T0).unwrap();
        s.put_ref(id, &ConvRef { reference: "r".into(), account: "a".into(), folder: "f".into(), uid: 1, uidvalidity: 1 }).unwrap();
        s.record_egress(&egress(Some(id), "{}"), T0).unwrap();
    }
    assert!(s.delete_conversation(c).unwrap());
    assert!(!s.delete_conversation(c).unwrap());
    for t in ["chat_messages", "conv_refs", "egress", "conversations"] {
        assert_eq!(count(&s, t), 1, "{t}");
    }
}

#[test]
fn purge_account_removes_scoped_and_referencing_conversations() {
    let mut s = store();
    s.set_scope("a", "INBOX", true).unwrap();
    s.set_scope("b", "INBOX", true).unwrap();
    let scoped = s.create_conversation(&conv(&["a", "b"]), T0).unwrap();
    let referencing = s.create_conversation(&conv(&[]), T0).unwrap();
    s.put_ref(referencing, &ConvRef { reference: "r".into(), account: "a".into(), folder: "f".into(), uid: 1, uidvalidity: 1 }).unwrap();
    let untouched = s.create_conversation(&conv(&["b"]), T0).unwrap();
    s.append_message(scoped, "user", "[]", T0).unwrap();
    s.record_egress(&egress(Some(scoped), "{}"), T0).unwrap();
    s.record_egress(&egress(Some(untouched), "{}"), T0).unwrap();
    let rep = s.purge_account("a").unwrap();
    assert_eq!(rep.conversations_deleted, 2);
    assert!(s.get_conversation(scoped).unwrap().is_none());
    assert!(s.get_conversation(referencing).unwrap().is_none());
    assert!(s.get_conversation(untouched).unwrap().is_some());
    assert_eq!(s.list_scopes().unwrap(), vec![("b".into(), "INBOX".into(), true)]);
    assert_eq!(count(&s, "chat_messages"), 0);
    assert_eq!(count(&s, "egress"), 1);
    // account names with SQL metacharacters are inert
    s.purge_account("x' OR '1'='1").unwrap();
    assert!(s.get_conversation(untouched).unwrap().is_some());
}

#[test]
fn purge_everything_clears_history_keeps_config() {
    let mut s = store();
    s.save_profile(&pin("a", "https://x.example"), T0).unwrap();
    let c = s.create_conversation(&conv(&[]), T0).unwrap();
    s.record_egress(&egress(Some(c), "{}"), T0).unwrap();
    s.add_usage(T0, "a", "m", 1, 1).unwrap();
    let r = s.purge_everything().unwrap();
    assert_eq!(r, PurgeReport { egress_deleted: 1, conversations_deleted: 1 });
    assert_eq!(s.list_profiles().unwrap().len(), 1);
    assert_eq!(count(&s, "usage_daily"), 1);
}

#[test]
fn rename_and_list_order() {
    let mut s = store();
    let a = s.create_conversation(&conv(&[]), T0).unwrap();
    let b = s.create_conversation(&conv(&[]), T0 + 5).unwrap();
    assert_eq!(s.list_conversations().unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), vec![b, a]);
    s.rename_conversation(a, "new").unwrap();
    assert_eq!(s.get_conversation(a).unwrap().unwrap().title, "new");
    assert!(matches!(s.rename_conversation(999, "x"), Err(StoreError::NotFound)));
}

#[test]
fn store_and_profile_debug_have_no_secrets() {
    let s = store();
    assert_eq!(format!("{s:?}"), "Store { .. }");
}

#[test]
fn refs_and_tool_runs_roundtrip_and_follow_the_conversation() {
    let mut s = store();
    let c = s.create_conversation(&conv(&["a@x"]), T0).unwrap();
    let other = s.create_conversation(&conv(&["a@x"]), T0).unwrap();
    s.put_ref(c, &ConvRef { reference: "m:1".into(), account: "a@x".into(), folder: "INBOX".into(), uid: 5, uidvalidity: 7 }).unwrap();
    s.put_ref(other, &ConvRef { reference: "m:1".into(), account: "a@x".into(), folder: "Sent".into(), uid: 9, uidvalidity: 1 }).unwrap();
    let refs = s.list_refs(c).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!((refs[0].folder.as_str(), refs[0].uid), ("INBOX", 5));
    let run = ToolRun { call_id: "call_1".into(), name: "search_mail".into(), label: "from:bob".into(), summary: "3 results".into(), ok: true };
    s.put_tool_run(c, &run).unwrap();
    s.put_tool_run(c, &ToolRun { summary: "4 results".into(), ..run.clone() }).unwrap();
    let runs = s.list_tool_runs(c).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].summary, "4 results");
    assert!(s.list_tool_runs(other).unwrap().is_empty());
    let mut e = egress(Some(c), "{}");
    e.round = 2;
    s.record_egress(&e, T0).unwrap();
    assert_eq!(s.list_egress(Some(c), 10).unwrap()[0].round, 2);
    s.delete_conversation(c).unwrap();
    assert!(s.list_refs(c).unwrap().is_empty() && s.list_tool_runs(c).unwrap().is_empty());
    assert_eq!(s.list_refs(other).unwrap().len(), 1);
}

#[test]
fn revoking_folder_grants_fails_closed_and_never_migrates() {
    let mut s = store();
    s.set_scope("a@x.org", "Projects", true).unwrap();
    s.set_scope("a@x.org", "Projects/Alpha", true).unwrap();
    s.set_scope("a@x.org", "Other", true).unwrap();
    s.set_scope("b@x.org", "Projects", true).unwrap();
    let mut c = conv(&["a@x.org"]);
    c.consent_json = r#"{"scopes":[{"account":"a@x.org","folder":"Projects","allowed":true},{"account":"a@x.org","folder":"Other","allowed":true},{"account":"b@x.org","folder":"Projects","allowed":true}],"tools":true}"#.into();
    let id = s.create_conversation(&c, T0).unwrap();
    s.add_conv_folder(id, "a@x.org", "Projects/Alpha").unwrap();
    s.put_ref(id, &ConvRef { reference: "r1".into(), account: "a@x.org".into(), folder: "Projects".into(), uid: 1, uidvalidity: 1 }).unwrap();
    s.put_ref(id, &ConvRef { reference: "r2".into(), account: "a@x.org".into(), folder: "Other".into(), uid: 2, uidvalidity: 1 }).unwrap();

    let n = s.revoke_folder_grants("a@x.org", &["Projects".into(), "Projects/Alpha".into()]).unwrap();
    assert_eq!(n, 4, "2 scopes + 1 consent entry + 1 created folder");
    let sc = s.list_scopes().unwrap();
    assert!(sc.contains(&("a@x.org".into(), "Projects".into(), false)));
    assert!(sc.contains(&("a@x.org".into(), "Projects/Alpha".into(), false)));
    assert!(sc.contains(&("a@x.org".into(), "Other".into(), true)), "unrelated folder keeps its grant");
    assert!(sc.contains(&("b@x.org".into(), "Projects".into(), true)), "other account keeps its grant");
    assert!(!sc.iter().any(|(_, f, a)| *a && f == "Projects/Gamma"), "no grant appears under a new name");
    let v: serde_json::Value = serde_json::from_str(&s.get_conversation(id).unwrap().unwrap().consent_json).unwrap();
    let allowed = |a: &str, f: &str| v["scopes"].as_array().unwrap().iter().any(|x| x["account"] == a && x["folder"] == f && x["allowed"] == true);
    assert!(!allowed("a@x.org", "Projects"));
    assert!(allowed("a@x.org", "Other"));
    assert!(allowed("b@x.org", "Projects"));
    assert_eq!(v["tools"], true, "other consent fields survive");
    assert!(s.list_conv_folders(id).unwrap().is_empty());
    assert!(s.get_ref(id, "r1").unwrap().is_none());
    assert!(s.get_ref(id, "r2").unwrap().is_some());
    assert_eq!(s.revoke_folder_grants("a@x.org", &["Projects".into()]).unwrap(), 0, "idempotent");
}
