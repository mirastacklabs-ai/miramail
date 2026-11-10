//! propose_rule_change: turn an existing rule on/off or discard it, only after Approve; re-checked at approval;
//! Undo puts it back. In-memory stores and fakes only.
mod support;

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use aichat::proposals::*;
use aichat::rules::{self, NewRule};
use aichat::redact::RedactOpts;
use aichat::store::{NewConversation, Store};
use aichat::tools::*;
use serde_json::json;
use support::actions::{FakeActions, ACC};

const T0: i64 = 1_000_000;

/// Folder list comes from the fake "server" (so a new folder appears there the moment it exists).
struct ServerHost(Arc<FakeActions>);

impl MailToolHost for ServerHost {
    fn accounts(&self) -> BoxFut<'_, Result<Vec<String>, String>> {
        Box::pin(async { Ok(vec![ACC.to_string()]) })
    }
    fn folders(&self, _account: &str) -> BoxFut<'_, Result<Vec<FolderMeta>, String>> {
        let mut names: Vec<String> = self.0.st.lock().unwrap().validity.keys().map(|(_, f)| f.clone()).collect();
        names.sort();
        Box::pin(async move { Ok(names.into_iter().map(|name| FolderMeta { name, role: "other".into(), total: 1, unread: 0 }).collect()) })
    }
    fn folder_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        self.0.folder_validity(account, folder)
    }
    fn search(&self, _s: &[Scope], _q: &str, _l: u32, _o: u32) -> BoxFut<'_, Result<SearchOut, String>> {
        Box::pin(async { Err("unused".into()) })
    }
    fn server_search(&self, _s: &Scope, _v: Option<u32>, _q: &ServerSearchArgs, _l: u32) -> BoxFut<'_, Result<Vec<MsgMeta>, String>> {
        Box::pin(async { Err("unused".into()) })
    }
    fn stats(&self, _s: &[Scope]) -> BoxFut<'_, Result<StatsOut, String>> {
        Box::pin(async { Err("unused".into()) })
    }
    fn get_message(&self, _s: &Scope, _u: u32, _v: u32) -> BoxFut<'_, Result<MsgText, String>> {
        Box::pin(async { Err("unused".into()) })
    }
    fn get_thread(&self, _s: &[Scope], _sc: &Scope, _u: u32, _v: u32, _l: u32) -> BoxFut<'_, Result<Vec<ThreadItem>, String>> {
        Box::pin(async { Err("unused".into()) })
    }
}

struct Rig {
    store: Arc<Mutex<Store>>,
    fake: Arc<FakeActions>,
    conv: i64,
    exec: Executor,
    hook: Arc<Hook>,
}

/// Stands in for the app's rule-enable step: records the call and starts a cursor like `RuleRunner::baseline`.
struct Hook {
    store: Arc<Mutex<Store>>,
    calls: Mutex<Vec<i64>>,
    /// Whether the rule was already ON when the baseline ran (it must not be).
    on_at_call: Mutex<Vec<bool>>,
}

impl RuleHook for Hook {
    fn baseline(&self, rule_id: i64) -> Result<(), String> {
        self.calls.lock().unwrap().push(rule_id);
        let on = self.store.lock().unwrap().get_rule(rule_id).unwrap().map(|r| r.enabled).unwrap_or(false);
        self.on_at_call.lock().unwrap().push(on);
        self.store.lock().unwrap().set_cursor(rule_id, ACC, "INBOX", rules::Cursor { uidvalidity: 7, last_uid: 99 }).map_err(|_| "x".to_string())
    }
}

fn rig() -> Rig {
    let mut st = Store::open_in_memory().unwrap();
    let conv = st
        .create_conversation(&NewConversation { title: "t".into(), profile_id: None, endpoint_host: "h".into(), model: "m".into(), consent_json: "{}".into(), accounts: vec![ACC.into()] }, T0)
        .unwrap();
    let store = Arc::new(Mutex::new(st));
    let fake = Arc::new(FakeActions::new());
    fake.add("INBOX", 1, "Invoice", false);
    let now = Arc::new(AtomicI64::new(T0));
    let hook = Arc::new(Hook { store: store.clone(), calls: Mutex::new(vec![]), on_at_call: Mutex::new(vec![]) });
    let exec = Executor::new(store.clone(), fake.clone(), Arc::new(move || now.load(Ordering::SeqCst)), Arc::new(Mutex::new(HashSet::new()))).with_rule_hook(hook.clone());
    Rig { store, fake, conv, exec, hook }
}

fn ticked() -> Vec<Scope> {
    vec![Scope { account: ACC.into(), folder: "INBOX".into() }, Scope { account: ACC.into(), folder: "Receipts".into() }]
}

/// What the app does at the start of every turn: rebuild the setup from the consent, then a fresh runtime.
fn turn(r: &Rig) -> ToolRuntime {
    let setup = ToolSetup {
        host: Arc::new(ServerHost(r.fake.clone())),
        scopes: ticked(),
        redaction: RedactOpts::default(),
        actions: Some(r.fake.clone()),
        drafts: None,
        calendar: None,
    };
    ToolRuntime::new(setup, r.store.clone(), r.conv)
}


const SPEC: &str = r#"{"v":1,"match":"all","conditions":[{"field":"from","op":"contains","value":"shop"}],"actions":[{"type":"moveTo","folder":"Receipts"},{"type":"markRead"}]}"#;

fn make_rule(r: &Rig, account: &str, name: &str, enabled: bool) -> i64 {
    let spec = rules::parse_spec(SPEC).unwrap();
    r.store.lock().unwrap().create_rule(&NewRule { account: account.into(), name: name.into(), spec, origin: "user", conv_id: None, reason: String::new(), enabled }, T0).unwrap()
}

fn get(r: &Rig, id: i64) -> Option<rules::Rule> {
    r.store.lock().unwrap().get_rule(id).unwrap()
}

async fn change(rt: &mut ToolRuntime, rule: i64, what: &str) -> aichat::tools::ToolOutcome {
    rt.execute("list_accounts", &json!({})).await;
    rt.execute("propose_rule_change", &json!({"account":"a:1","rule":rule,"change":what,"reason":"x"})).await
}

async fn plan(r: &Rig, rule: i64, what: &str) -> i64 {
    let mut rt = turn(r);
    let o = change(&mut rt, rule, what).await;
    assert!(o.ok, "{}", o.content);
    rt.take_new_proposals()[0].0
}

#[tokio::test]
async fn planning_writes_the_card_from_the_stored_rule_and_changes_nothing() {
    let r = rig();
    let off = make_rule(&r, ACC, "GST to Receipts", false);
    let on = make_rule(&r, ACC, "Other", true);
    let mut rt = turn(&r);
    let o = change(&mut rt, off, "enable").await;
    assert!(o.ok, "{}", o.content);
    let o = change(&mut rt, on, "disable").await;
    assert!(o.ok, "{}", o.content);
    let o = change(&mut rt, on, "discard").await;
    assert!(o.ok, "{}", o.content);
    let titles: Vec<String> = rt.take_new_proposals().into_iter().map(|p| p.1).collect();
    assert_eq!(titles, vec!["Turn on rule \u{201c}GST to Receipts\u{201d}", "Turn off rule \u{201c}Other\u{201d}", "Remove rule \u{201c}Other\u{201d}"]);
    let p = r.store.lock().unwrap().list_proposals(Some(r.conv), 10).unwrap();
    let enable = p.iter().find(|x| x.title.starts_with("Turn on")).unwrap();
    assert!(enable.warnings.iter().any(|w| w.starts_with("What the rule does:")), "{:?}", enable.warnings);
    assert!(enable.warnings.iter().any(|w| w.contains("only mail that arrives after")), "{:?}", enable.warnings);
    // nothing changed
    assert!(!get(&r, off).unwrap().enabled && get(&r, on).unwrap().enabled);
    assert!(r.hook.calls.lock().unwrap().is_empty());
}

fn make_spec_rule(r: &Rig, name: &str, enabled: bool, spec_json: &str) -> i64 {
    let spec = rules::parse_spec(spec_json).unwrap();
    r.store.lock().unwrap().create_rule(&NewRule { account: ACC.into(), name: name.into(), spec, origin: "user", conv_id: None, reason: String::new(), enabled }, T0).unwrap()
}

fn rt_with(r: &Rig, scopes: Vec<Scope>) -> ToolRuntime {
    let setup = ToolSetup { host: Arc::new(ServerHost(r.fake.clone())), scopes, redaction: RedactOpts::default(), actions: Some(r.fake.clone()), drafts: None, calendar: None };
    ToolRuntime::new(setup, r.store.clone(), r.conv)
}

const OUTSIDE: &str = "outside the folders the user allowed";

#[tokio::test]
async fn a_rule_that_touches_a_folder_outside_the_scope_is_refused_whatever_the_change() {
    let r = rig();
    // watches Secret (exists, not ticked)
    let watch = make_spec_rule(&r, "Watch", true, r#"{"v":1,"match":"all","conditions":[{"field":"folder","op":"equals","value":"Secret"},{"field":"from","op":"contains","value":"x"}],"actions":[{"type":"markRead"}]}"#);
    // moves into Secret
    let mv = make_spec_rule(&r, "Move", true, r#"{"v":1,"match":"all","conditions":[{"field":"from","op":"contains","value":"x"}],"actions":[{"type":"moveTo","folder":"secret"}]}"#);
    // glob that also matches Secret
    let globbed = make_spec_rule(&r, "Glob", true, r#"{"v":1,"match":"all","conditions":[{"field":"folder","op":"glob","value":"Sec*"},{"field":"from","op":"contains","value":"x"}],"actions":[{"type":"markRead"}]}"#);
    let mut rt = turn(&r);
    for id in [watch, mv, globbed] {
        for what in ["disable", "discard"] {
            let o = change(&mut rt, id, what).await;
            assert!(!o.ok && o.content.contains("This rule uses the folder Secret, which is outside the folders the user allowed"), "{id} {what}: {}", o.content);
        }
    }
    assert!(rt.take_new_proposals().is_empty());
}

#[tokio::test]
async fn a_rule_without_a_folder_condition_needs_the_inbox_in_scope() {
    let r = rig();
    let id = make_rule(&r, ACC, "Inbox rule", true);
    let mut rt = rt_with(&r, vec![Scope { account: ACC.into(), folder: "Receipts".into() }]);
    let o = change(&mut rt, id, "disable").await;
    assert!(!o.ok && o.content.contains(OUTSIDE) && o.content.contains("INBOX"), "{}", o.content);
    let mut rt = turn(&r);
    assert!(change(&mut rt, id, "disable").await.ok);
}

#[tokio::test]
async fn consent_narrowed_between_plan_and_approve_refuses_the_change() {
    let r = rig();
    let id = make_rule(&r, ACC, "R", true);
    let pid = plan(&r, id, "disable").await;
    // INBOX is still allowed, but the moveTo folder (Receipts) no longer is
    let only_inbox = vec![Scope { account: ACC.into(), folder: "INBOX".into() }];
    let rep = r.exec.approve(pid, &[], &only_inbox, &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.skipped), (State::Failed, 0, 1));
    assert!(get(&r, id).unwrap().enabled);
    let p = r.store.lock().unwrap().get_proposal(pid).unwrap().unwrap();
    assert!(p.error.contains(OUTSIDE), "{}", p.error);
}

#[tokio::test]
async fn a_rule_naming_a_vanished_folder_cannot_be_turned_on_but_can_be_turned_off_or_removed() {
    let r = rig();
    let gone = r#"{"v":1,"match":"all","conditions":[{"field":"folder","op":"equals","value":"INBOX"},{"field":"from","op":"contains","value":"x"}],"actions":[{"type":"moveTo","folder":"Gone"}]}"#;
    let off = make_spec_rule(&r, "Off", false, gone);
    let on = make_spec_rule(&r, "On", true, gone);
    let mut rt = turn(&r);
    let o = change(&mut rt, off, "enable").await;
    assert!(!o.ok && o.content.contains("no longer exists"), "{}", o.content);
    assert!(change(&mut rt, on, "disable").await.ok);
    assert!(change(&mut rt, on, "discard").await.ok);
    let ids: Vec<i64> = rt.take_new_proposals().into_iter().map(|p| p.0).collect();
    let rep = r.exec.approve(ids[0], &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 1));
    assert!(!get(&r, on).unwrap().enabled);
}

#[tokio::test]
async fn planning_refuses_unknown_foreign_out_of_scope_and_noop() {
    let r = rig();
    let off = make_rule(&r, ACC, "Off", false);
    let on = make_rule(&r, ACC, "On", true);
    let foreign = make_rule(&r, "other@example.org", "Foreign", false);
    let mut rt = turn(&r);
    for (rule, what, needle) in [(999, "enable", "does not exist"), (foreign, "enable", "does not exist"), (on, "enable", "already on"), (off, "disable", "already off")] {
        let o = change(&mut rt, rule, what).await;
        assert!(!o.ok && o.content.contains(needle), "{rule} {what}: {}", o.content);
    }
    // an account that is not in the conversation
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_rule_change", &json!({"account":"a:9","rule":off,"change":"enable"})).await;
    assert!(!o.ok, "{}", o.content);
    // bad enum / extra field are rejected by the schema
    let o = rt.execute("propose_rule_change", &json!({"account":"a:1","rule":off,"change":"delete"})).await;
    assert!(!o.ok);
    assert!(rt.take_new_proposals().is_empty());
}

#[tokio::test]
async fn approve_enable_uses_the_ui_enable_path_and_undo_turns_it_off() {
    let r = rig();
    let id = make_rule(&r, ACC, "R", false);
    let pid = plan(&r, id, "enable").await;
    assert!(!get(&r, id).unwrap().enabled);
    let rep = r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 1));
    assert!(get(&r, id).unwrap().enabled);
    assert_eq!(*r.hook.calls.lock().unwrap(), vec![id]);
    let c = r.store.lock().unwrap().get_cursor(id, ACC, "INBOX").unwrap().unwrap();
    assert_eq!(c.last_uid, 99, "cursor started at now, like the Rules screen");
    let log = r.store.lock().unwrap().list_log(pid).unwrap();
    let p = r.store.lock().unwrap().get_proposal(pid).unwrap().unwrap();
    assert!(undoable(&p, &log));
    let u = r.exec.undo(pid, &|_| {}).await.unwrap();
    assert_eq!((u.state, u.undone, u.failed), (State::Undone, 1, 0));
    assert!(!get(&r, id).unwrap().enabled);
}

#[tokio::test]
async fn approve_disable_then_undo_turns_it_back_on_through_the_hook() {
    let r = rig();
    let id = make_rule(&r, ACC, "R", true);
    let pid = plan(&r, id, "disable").await;
    let rep = r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 1));
    assert!(!get(&r, id).unwrap().enabled && r.hook.calls.lock().unwrap().is_empty());
    let u = r.exec.undo(pid, &|_| {}).await.unwrap();
    assert_eq!(u.state, State::Undone);
    assert!(get(&r, id).unwrap().enabled);
    assert_eq!(*r.hook.calls.lock().unwrap(), vec![id]);
}

#[tokio::test]
async fn discard_removes_the_rule_and_undo_restores_it_with_a_new_id() {
    let r = rig();
    let id = make_rule(&r, ACC, "Keep me", true);
    let pid = plan(&r, id, "discard").await;
    assert!(get(&r, id).is_some(), "proposing removes nothing");
    let rep = r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 1));
    assert!(get(&r, id).is_none());
    let log = r.store.lock().unwrap().list_log(pid).unwrap();
    assert_eq!(log[0].op, "rule_discard");
    let u = r.exec.undo(pid, &|_| {}).await.unwrap();
    assert_eq!((u.state, u.undone, u.failed), (State::Undone, 1, 0));
    let all = r.store.lock().unwrap().list_rules(Some(ACC)).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!((all[0].name.as_str(), all[0].enabled, all[0].spec_json.contains("Receipts")), ("Keep me", true, true));
    assert_ne!(all[0].id, id, "restored under a new id");
    assert_eq!(*r.hook.calls.lock().unwrap(), vec![all[0].id], "restored-as-on rules start at now too");
}

#[tokio::test]
async fn approval_refuses_when_the_rule_changed_vanished_or_the_consent_narrowed() {
    // edited after planning
    let r = rig();
    let id = make_rule(&r, ACC, "R", false);
    let pid = plan(&r, id, "enable").await;
    let spec = rules::parse_spec(SPEC).unwrap();
    r.store.lock().unwrap().update_rule(id, "Renamed", &spec, T0 + 5).unwrap();
    let rep = r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Failed, 0));
    assert!(!get(&r, id).unwrap().enabled && r.hook.calls.lock().unwrap().is_empty());

    // toggled by the user after planning
    let r = rig();
    let id = make_rule(&r, ACC, "R", false);
    let pid = plan(&r, id, "enable").await;
    r.store.lock().unwrap().set_rule_enabled(id, true, T0 + 5).unwrap();
    let rep = r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!(rep.state, State::Failed);

    // deleted by the user after planning
    let r = rig();
    let id = make_rule(&r, ACC, "R", true);
    let pid = plan(&r, id, "disable").await;
    r.store.lock().unwrap().delete_rule(id).unwrap();
    let rep = r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!(rep.state, State::Failed);

    // consent narrowed: the account is no longer in scope
    let r = rig();
    let id = make_rule(&r, ACC, "R", true);
    let pid = plan(&r, id, "discard").await;
    let rep = r.exec.approve(pid, &[], &[Scope { account: "other@example.org".into(), folder: "INBOX".into() }], &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.skipped), (State::Failed, 1));
    assert!(get(&r, id).is_some());
}

#[tokio::test]
async fn undo_leaves_a_rule_alone_that_was_edited_since() {
    let r = rig();
    let id = make_rule(&r, ACC, "R", false);
    let pid = plan(&r, id, "enable").await;
    r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    let spec = rules::parse_spec(SPEC).unwrap();
    r.store.lock().unwrap().update_rule(id, "Edited", &spec, T0 + 9).unwrap(); // editing switches it off
    r.store.lock().unwrap().set_rule_enabled(id, true, T0 + 10).unwrap(); // the user turned the edited rule on again
    let u = r.exec.undo(pid, &|_| {}).await.unwrap();
    assert_eq!((u.undone, u.failed), (0, 1));
    assert_ne!(u.state, State::Undone);
}

#[tokio::test]
async fn old_params_without_rule_change_still_deserialize() {
    let p: Params = serde_json::from_str(r#"{"account":"a@x.org","new_folder":"Tax"}"#).unwrap();
    assert!(p.rule_change.is_none());
}

#[tokio::test]
async fn baseline_runs_before_the_rule_is_switched_on() {
    let r = rig();
    let id = make_rule(&r, ACC, "R", false);
    let pid = plan(&r, id, "enable").await;
    r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!(*r.hook.on_at_call.lock().unwrap(), vec![false]);
    assert!(get(&r, id).unwrap().enabled);
}

#[test]
fn conditional_write_refuses_a_rule_that_changed() {
    let r = rig();
    let id = make_rule(&r, ACC, "R", false);
    let seen = get(&r, id).unwrap();
    let spec = rules::parse_spec(SPEC).unwrap();
    r.store.lock().unwrap().update_rule(id, "Edited", &spec, T0 + 1).unwrap();
    assert!(!r.store.lock().unwrap().set_rule_enabled_matching(&seen, true, T0 + 2).unwrap());
    assert!(!r.store.lock().unwrap().delete_rule_matching(&seen).unwrap());
    let now = get(&r, id).unwrap();
    assert!(!now.enabled);
    assert!(r.store.lock().unwrap().set_rule_enabled_matching(&now, true, T0 + 3).unwrap());
    let cur = get(&r, id).unwrap();
    assert!(r.store.lock().unwrap().delete_rule_matching(&cur).unwrap());
}

#[tokio::test]
async fn undo_of_a_rule_already_in_the_target_state_counts_as_undone() {
    let r = rig();
    let id = make_rule(&r, ACC, "R", false);
    let pid = plan(&r, id, "enable").await;
    r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    r.store.lock().unwrap().set_rule_enabled(id, false, T0 + 4).unwrap(); // the user turned it off themselves
    let u = r.exec.undo(pid, &|_| {}).await.unwrap();
    assert_eq!((u.state, u.undone, u.failed), (State::Undone, 1, 0));
    assert!(!get(&r, id).unwrap().enabled);
}

#[tokio::test]
async fn a_second_undo_of_a_discard_cannot_duplicate_the_rule() {
    let r = rig();
    let id = make_rule(&r, ACC, "Once", false);
    let pid = plan(&r, id, "discard").await;
    r.exec.approve(pid, &[], &ticked(), &|_| {}).await.unwrap();
    r.exec.undo(pid, &|_| {}).await.unwrap();
    assert!(r.exec.undo(pid, &|_| {}).await.is_err());
    assert_eq!(r.store.lock().unwrap().list_rules(Some(ACC)).unwrap().len(), 1);
}

#[test]
fn unknown_change_gets_a_neutral_title() {
    let rc = |c: &str| Params { rule_change: Some(RuleChangeParams { rule_id: 1, account: ACC.into(), change: c.into(), name: "N".into(), fingerprint: String::new() }), ..Params::default() };
    assert_eq!(title_for(Kind::RuleChange, &rc("discard"), 0, ""), "Remove rule \u{201c}N\u{201d}");
    assert_eq!(title_for(Kind::RuleChange, &rc("zzz"), 0, ""), "Change rule \u{201c}N\u{201d}");
}
