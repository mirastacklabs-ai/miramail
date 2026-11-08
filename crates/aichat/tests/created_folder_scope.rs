//! A folder MIRA created after the user approved the create-folder card joins THAT conversation's allowed
//! set (and nothing else does). In-memory stores and fakes only.
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
    vec![Scope { account: ACC.into(), folder: "INBOX".into() }]
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

async fn propose_create(rt: &mut ToolRuntime, name: &str) -> i64 {
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_create_folder", &json!({"account":"a:1","name":name,"reason":"x"})).await;
    assert!(o.ok, "{}", o.content);
    rt.take_new_proposals()[0].0
}

async fn approve(r: &Rig, id: i64) {
    let rep = r.exec.approve(id, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!(rep.state, State::Done);
}

fn ref_of(text: &str, folder: &str) -> Option<String> {
    text.lines().find(|l| l.contains(&format!(" {folder} ["))).and_then(|l| l.split_whitespace().next()).map(str::to_string)
}

#[tokio::test]
async fn approved_create_folder_makes_the_folder_listed_movable_and_in_scope_for_that_conversation() {
    let r = rig();
    let mut rt = turn(&r);
    let before = folders_text(&mut rt).await;
    assert!(before.contains("INBOX") && !before.contains("Marketing"));
    let id = propose_create(&mut rt, "Marketing").await;
    // (1) proposed but not approved: still not listed, not even in a fresh turn
    assert!(!folders_text(&mut turn(&r)).await.contains("Marketing"));

    approve(&r, id).await;

    // (2) next turn: listed with a ref, and usable as a move destination
    r.store.lock().unwrap().put_ref(r.conv, &ConvRef { reference: "m:1".into(), account: ACC.into(), folder: "INBOX".into(), uid: 1, uidvalidity: 7 }).unwrap();
    let mut rt = turn(&r);
    let after = folders_text(&mut rt).await;
    let fref = ref_of(&after, "Marketing").unwrap_or_else(|| panic!("not listed: {after}"));
    assert!(fref.starts_with("f:"));
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_folder":fref,"reason":"x"})).await;
    assert!(o.ok, "{}", o.content);
    // (3) pre-existing unticked folders stay hidden
    for hidden in ["Receipts", "Secret", "Archive", "Trash"] {
        assert!(!after.contains(hidden), "{hidden} leaked: {after}");
    }
    // the model is told what to do next
    let mut rt = turn(&r);
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_create_folder", &json!({"account":"a:1","name":"Other"})).await;
    assert!(o.content.contains("list_folders"), "{}", o.content);
}

#[tokio::test]
async fn folders_created_any_other_way_stay_out_of_scope() {
    let r = rig();
    // (4) the user or a rule creates a folder on the server: not an approved card of this conversation
    assert!(r.fake.create_folder(ACC, "ByRule").await.unwrap());
    let text = folders_text(&mut turn(&r)).await;
    assert!(!text.contains("ByRule"), "{text}");
    // an approved card in ANOTHER conversation does not leak either
    let other = r.store.lock().unwrap().create_conversation(&NewConversation { title: "o".into(), profile_id: None, endpoint_host: "h".into(), model: "m".into(), consent_json: "{}".into(), accounts: vec![ACC.into()] }, T0).unwrap();
    r.store.lock().unwrap().add_conv_folder(other, ACC, "OtherConvOnly").unwrap();
    r.fake.st.lock().unwrap().validity.insert((ACC.into(), "OtherConvOnly".into()), 3);
    assert!(!folders_text(&mut turn(&r)).await.contains("OtherConvOnly"));
    // a card for a folder that already existed adds nothing
    let params = Params { account: Some(ACC.into()), new_folder: Some("Secret".into()), ..Params::default() };
    let plan = build_plan(Kind::CreateFolder, params, vec![], 0, 0).unwrap();
    let id = r.store.lock().unwrap().create_proposal(&NewProposal { conv_id: r.conv, kind: Kind::CreateFolder, params: plan.params, title: plan.title, reason: String::new(), warnings: vec![], items: vec![] }, T0).unwrap();
    let rep = r.exec.approve(id, &[], &ticked(), &|_| {}).await.unwrap();
    assert_eq!(rep.noop, 1);
    assert!(!folders_text(&mut turn(&r)).await.contains("Secret"));
}

#[tokio::test]
async fn the_created_folder_is_remembered_after_reload_and_dropped_with_account_or_conversation() {
    let r = rig();
    let id = propose_create(&mut turn(&r), "Marketing").await;
    approve(&r, id).await;
    // (5) "reload": brand-new runtimes built from the stored data only
    for _ in 0..2 {
        assert!(folders_text(&mut turn(&r)).await.contains("Marketing"));
    }
    assert_eq!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap(), vec![(ACC.to_string(), "Marketing".to_string())]);
    // an account that is no longer ticked in the consent is not resurrected by the stored row
    let mut setup = ToolSetup { host: Arc::new(ServerHost(r.fake.clone())), scopes: vec![], redaction: RedactOpts::default(), actions: None, drafts: None, calendar: None };
    setup.scopes.push(Scope { account: "other@x.org".into(), folder: "INBOX".into() });
    let mut rt = ToolRuntime::new(setup, r.store.clone(), r.conv);
    assert!(!rt.execute("list_folders", &json!({})).await.content.contains("Marketing"));
    r.store.lock().unwrap().purge_account(ACC).unwrap();
    assert!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap().is_empty());
}

#[tokio::test]
async fn a_move_that_creates_its_destination_adds_it_to_the_scope_only_when_it_really_created_it() {
    let r = rig();
    r.store.lock().unwrap().put_ref(r.conv, &ConvRef { reference: "m:1".into(), account: ACC.into(), folder: "INBOX".into(), uid: 1, uidvalidity: 7 }).unwrap();
    // (a) proposing alone adds nothing
    let mut rt = turn(&r);
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"Brand New","reason":"x"})).await;
    assert!(o.ok, "{}", o.content);
    let id = rt.take_new_proposals()[0].0;
    assert!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap().is_empty());
    // (b) the approved move creates it: recorded, listed next turn
    let rep = r.exec.approve(id, &[0], &ticked(), &|_| {}).await.unwrap();
    assert!(rep.folders_changed, "{rep:?}");
    assert_eq!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap(), vec![(ACC.to_string(), "Brand New".to_string())]);
    assert!(folders_text(&mut turn(&r)).await.contains("Brand New"));
    // (c) a move into a folder that already exists (unticked "Secret") records nothing
    r.fake.st.lock().unwrap().validity.insert((ACC.into(), "Secret".into()), 3);
    let mut rt = turn(&r);
    rt.execute("list_accounts", &json!({})).await;
    let o = rt.execute("propose_move", &json!({"refs":["m:1"],"to_new_folder":"Secret","reason":"x"})).await;
    if o.ok {
        let id2 = rt.take_new_proposals()[0].0;
        let _ = r.exec.approve(id2, &[0], &ticked(), &|_| {}).await;
    }
    assert_eq!(r.store.lock().unwrap().list_conv_folders(r.conv).unwrap().len(), 1);
}
