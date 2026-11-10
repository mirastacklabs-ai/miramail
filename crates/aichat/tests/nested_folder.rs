//! Creating a folder INSIDE an existing folder (propose_create_folder `parent`, propose_move `new_folder_parent`).
//! The parent must be in the conversation's scope at planning and at approval. In-memory stores and fakes only.
mod support;

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use aichat::proposals::*;
use aichat::redact::RedactOpts;
use aichat::store::{ConvRef, NewConversation, Store};
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
    let exec = Executor::new(store.clone(), fake.clone(), Arc::new(move || now.load(Ordering::SeqCst)), Arc::new(Mutex::new(HashSet::new())));
    Rig { store, fake, conv, exec }
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

async fn folders_text(rt: &mut ToolRuntime) -> String {
    rt.execute("list_accounts", &json!({})).await;
    rt.execute("list_folders", &json!({})).await.content
}

async fn create(rt: &mut ToolRuntime, args: serde_json::Value) -> aichat::tools::ToolOutcome {
    rt.execute("list_accounts", &json!({})).await;
    rt.execute("propose_create_folder", &args).await
}

fn put_msg(r: &Rig) {
    r.store.lock().unwrap().put_ref(r.conv, &ConvRef { reference: "m:1".into(), account: ACC.into(), folder: "INBOX".into(), uid: 1, uidvalidity: 7 }).unwrap();
}

#[tokio::test]
async fn parent_in_scope_by_name_or_reference_plans_the_full_name() {
    let r = rig();
    let mut rt = turn(&r);
    let o = create(&mut rt, json!({"account":"a:1","name":"GST","parent":"receipts","reason":"x"})).await;
    assert!(o.ok, "{}", o.content);
    let (id, title) = rt.take_new_proposals().remove(0);
    assert_eq!(title, "Create folder \"GST\" inside \"Receipts\"");
    let p = r.store.lock().unwrap().get_proposal(id).unwrap().unwrap();
    assert_eq!((p.params.new_folder.as_deref(), p.params.new_folder_parent.as_deref()), (Some("GST"), Some("Receipts")));
    assert!(r.fake.mutations().is_empty());

    // by f: reference
    let text = folders_text(&mut rt).await;
    let fref = text.lines().find(|l| l.contains(" Receipts [")).and_then(|l| l.split_whitespace().next()).unwrap().to_string();
    let o = rt.execute("propose_create_folder", &json!({"account":"a:1","name":"VAT","parent":fref})).await;
    assert!(o.ok, "{}", o.content);

    // approve: created under the full name, joins the conversation scope under it
    let rep = r.exec.approve(id, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.folders_changed), (State::Done, 1, true));
    assert_eq!(r.fake.mutations(), vec!["create Receipts/GST".to_string()]);
    assert_eq!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap(), vec![(ACC.to_string(), "Receipts/GST".to_string())]);
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    assert_eq!(log.iter().find(|l| l.op == "create_folder").unwrap().dest_folder, "Receipts/GST");
}

#[tokio::test]
async fn parent_out_of_scope_or_missing_or_colliding_is_refused_and_nothing_is_stored() {
    let r = rig();
    let mut rt = turn(&r);
    let o = create(&mut rt, json!({"account":"a:1","name":"GST","parent":"Secret"})).await;
    assert!(!o.ok && o.content.contains("outside the folders the user allowed"), "{}", o.content);
    let o = create(&mut rt, json!({"account":"a:1","name":"GST","parent":"NoSuch"})).await;
    assert!(!o.ok && o.content.contains("does not exist"), "{}", o.content);
    let o = create(&mut rt, json!({"account":"a:1","name":"GST","parent":"f:99"})).await;
    assert!(!o.ok, "{}", o.content);
    let o = create(&mut rt, json!({"account":"a:1","name":"a/b","parent":"Receipts"})).await;
    assert!(!o.ok, "{}", o.content);
    // collision on the full name (case-insensitive)
    r.fake.st.lock().unwrap().validity.insert((ACC.into(), "Receipts/GST".into()), 3);
    let o = create(&mut rt, json!({"account":"a:1","name":"gst","parent":"Receipts"})).await;
    assert!(!o.ok && o.content.contains("already exists"), "{}", o.content);
    assert!(rt.take_new_proposals().is_empty());
    assert!(r.fake.mutations().is_empty());
}

#[tokio::test]
async fn move_into_a_new_subfolder_plans_and_creates_the_full_name() {
    let r = rig();
    put_msg(&r);
    let mut rt = turn(&r);
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_folder":"f:1","new_folder_parent":"Receipts"})).await;
    assert!(!o.ok && o.content.contains("only be used together"), "{}", o.content);
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"new_folder_parent":"Receipts"})).await;
    assert!(!o.ok, "{}", o.content);
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"GST","new_folder_parent":"Secret"})).await;
    assert!(!o.ok && o.content.contains("outside the folders the user allowed"), "{}", o.content);
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"GST","new_folder_parent":"NoSuch"})).await;
    assert!(!o.ok, "{}", o.content);
    assert!(rt.take_new_proposals().is_empty());

    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"GST","new_folder_parent":"Receipts"})).await;
    assert!(o.ok, "{}", o.content);
    let (id, title) = rt.take_new_proposals().remove(0);
    assert_eq!(title, "Move 1 message to Receipts/GST");
    let p = r.store.lock().unwrap().get_proposal(id).unwrap().unwrap();
    assert!(p.warnings.iter().any(|w| w.contains("inside \"Receipts\"")), "{:?}", p.warnings);
    assert_eq!((p.params.dest_folder.as_deref(), p.params.create_dest, p.params.new_folder_parent.as_deref()), (Some("Receipts/GST"), true, Some("Receipts")));
    let rep = r.exec.approve(id, &[0], &ticked(), &|_| {}).await.unwrap();
    assert!(rep.folders_changed, "{rep:?}");
    assert_eq!(r.fake.mutations()[0], "create Receipts/GST");
    assert_eq!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap(), vec![(ACC.to_string(), "Receipts/GST".to_string())]);
}

#[tokio::test]
async fn move_refuses_an_existing_full_name_that_is_not_in_scope() {
    let r = rig();
    put_msg(&r);
    r.fake.st.lock().unwrap().validity.insert((ACC.into(), "Receipts/GST".into()), 3);
    let mut rt = turn(&r);
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"GST","new_folder_parent":"Receipts"})).await;
    assert!(!o.ok && o.content.contains("outside the folders the user allowed"), "{}", o.content);
}

#[tokio::test]
async fn approval_rechecks_that_the_parent_is_still_allowed_and_exists() {
    let only_inbox = vec![Scope { account: ACC.into(), folder: "INBOX".into() }];
    let r = rig();
    let mut rt = turn(&r);
    let o = create(&mut rt, json!({"account":"a:1","name":"GST","parent":"Receipts"})).await;
    assert!(o.ok, "{}", o.content);
    let id = rt.take_new_proposals()[0].0;
    let rep = r.exec.approve(id, &[], &only_inbox, &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.skipped), (State::Failed, 0, 1));
    assert!(r.fake.mutations().is_empty());
    assert!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap().is_empty());

    let r = rig();
    let mut rt = turn(&r);
    let o = create(&mut rt, json!({"account":"a:1","name":"GST","parent":"Receipts"})).await;
    assert!(o.ok, "{}", o.content);
    let id = rt.take_new_proposals()[0].0;
    r.fake.st.lock().unwrap().validity.remove(&(ACC.to_string(), "Receipts".to_string()));
    let rep = r.exec.approve(id, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Failed, 0));
    assert!(r.fake.mutations().is_empty());

    let r = rig();
    put_msg(&r);
    let mut rt = turn(&r);
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"GST","new_folder_parent":"Receipts"})).await;
    assert!(o.ok, "{}", o.content);
    let id = rt.take_new_proposals()[0].0;
    let rep = r.exec.approve(id, &[0], &only_inbox, &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.folders_changed), (State::Failed, 0, false));
    assert!(r.fake.mutations().is_empty(), "{:?}", r.fake.mutations());
    assert!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap().is_empty());
}

#[test]
fn old_stored_params_without_a_parent_still_deserialize() {
    let old = r#"{"dest_account":null,"dest_folder":null,"create_dest":false,"flag":null,"account":"a@x.org","new_folder":"Tax","cal":null}"#;
    let p: Params = serde_json::from_str(old).unwrap();
    assert_eq!((p.new_folder.as_deref(), p.new_folder_parent.clone()), (Some("Tax"), None));
    let plan = build_plan(Kind::CreateFolder, p, vec![], 0, 0).unwrap();
    assert_eq!(plan.title, "Create folder \"Tax\"");
}

fn move_proposal(r: &Rig, args: serde_json::Value) -> i64 {
    // synchronous helper is not possible (execute is async); see callers
    let _ = (r, args);
    0
}

async fn plan_move(r: &Rig, args: serde_json::Value) -> i64 {
    let _ = move_proposal;
    put_msg(r);
    let mut rt = turn(r);
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_move", &args).await;
    assert!(o.ok, "{}", o.content);
    rt.take_new_proposals()[0].0
}

#[tokio::test]
async fn approve_refuses_a_new_folder_destination_that_appeared_meanwhile_outside_the_scope() {
    let r = rig();
    let id = plan_move(&r, json!({"refs":["m:1"],"to_new_folder":"Brand","reason":"x"})).await;
    r.fake.st.lock().unwrap().validity.insert((ACC.into(), "Brand".into()), 5); // someone made it; not in scope
    let rep = r.exec.approve(id, &[0], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Failed, 0));
    assert!(r.fake.mutations().is_empty(), "{:?}", r.fake.mutations());
    assert_eq!(r.fake.uids("INBOX"), vec![1]);
    assert!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap().is_empty());
}

#[tokio::test]
async fn approve_reuses_a_meanwhile_folder_in_scope_with_the_servers_spelling() {
    let r = rig();
    let id = plan_move(&r, json!({"refs":["m:1"],"to_new_folder":"Later","reason":"x"})).await;
    r.fake.st.lock().unwrap().validity.insert((ACC.into(), "LATER".into()), 5);
    let mut sc = ticked();
    sc.push(Scope { account: ACC.into(), folder: "LATER".into() });
    let rep = r.exec.approve(id, &[0], &sc, &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 1));
    assert_eq!(r.fake.uids("LATER").len(), 1);
    assert!(r.fake.uids("Later").is_empty());
}

#[tokio::test]
async fn approve_rechecks_a_to_folder_destination_against_the_reread_scope() {
    let r = rig();
    put_msg(&r);
    let mut rt = turn(&r);
    let text = folders_text(&mut rt).await;
    let fref = text.lines().find(|l| l.contains(" Receipts [")).and_then(|l| l.split_whitespace().next()).unwrap().to_string();
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_folder":fref,"reason":"x"})).await;
    assert!(o.ok, "{}", o.content);
    let id = rt.take_new_proposals()[0].0;
    // consent narrowed to INBOX only: the source is still allowed, the destination is not
    let only_inbox = vec![Scope { account: ACC.into(), folder: "INBOX".into() }];
    let rep = r.exec.approve(id, &[0], &only_inbox, &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Failed, 0));
    assert!(r.fake.mutations().is_empty());
    // the normal path still works
    let r = rig();
    put_msg(&r);
    let mut rt = turn(&r);
    let text = folders_text(&mut rt).await;
    let fref = text.lines().find(|l| l.contains(" Receipts [")).and_then(|l| l.split_whitespace().next()).unwrap().to_string();
    rt.execute("propose_move", &json!({"refs":["m:1"],"to_folder":fref})).await;
    let id = rt.take_new_proposals()[0].0;
    let rep = r.exec.approve(id, &[0], &ticked(), &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 1));
    assert_eq!(r.fake.uids("Receipts").len(), 1);
}

#[tokio::test]
async fn role_folders_are_refused_as_parents_at_plan_and_approval() {
    let r = rig();
    put_msg(&r);
    let mut sc = ticked();
    sc.push(Scope { account: ACC.into(), folder: "Trash".into() });
    let setup = ToolSetup { host: Arc::new(ServerHost(r.fake.clone())), scopes: sc.clone(), redaction: RedactOpts::default(), actions: Some(r.fake.clone()), drafts: None, calendar: None };
    let mut rt = ToolRuntime::new(setup, r.store.clone(), r.conv);
    let o = create(&mut rt, json!({"account":"a:1","name":"X","parent":"Trash"})).await;
    assert!(!o.ok && o.content.contains("special folder"), "{}", o.content);
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"X","new_folder_parent":"Trash"})).await;
    assert!(!o.ok && o.content.contains("special folder"), "{}", o.content);
    // INBOX stays allowed
    let o = create(&mut rt, json!({"account":"a:1","name":"Sub","parent":"INBOX"})).await;
    assert!(o.ok, "{}", o.content);
    // a stored card that names a role folder is refused at approval
    let params = Params { account: Some(ACC.into()), new_folder: Some("X".into()), new_folder_parent: Some("Trash".into()), ..Params::default() };
    let pl = build_plan(Kind::CreateFolder, params, vec![], 0, 0).unwrap();
    let id = r.store.lock().unwrap().create_proposal(&NewProposal { conv_id: r.conv, kind: Kind::CreateFolder, params: pl.params, title: pl.title, reason: String::new(), warnings: vec![], items: vec![] }, T0).unwrap();
    let rep = r.exec.approve(id, &[], &sc, &|_| {}).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Failed, 0));
    assert!(r.fake.mutations().is_empty());
}
