//! Proposal state machine, executor revalidation, chunking, partial failure and undo, against an
//! in-memory fake mail host. No network, no real mailbox, no Keychain.
mod support;

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use aichat::proposals::*;
use aichat::store::{NewConversation, Store};
use aichat::tools::Scope;
use support::actions::{FakeActions, ACC};

const T0: i64 = 1_000_000;

struct Rig {
    store: Arc<Mutex<Store>>,
    fake: Arc<FakeActions>,
    now: Arc<AtomicI64>,
    exec: Executor,
    conv: i64,
}

fn rig() -> Rig {
    let mut st = Store::open_in_memory().unwrap();
    let conv = st
        .create_conversation(
            &NewConversation { title: "t".into(), profile_id: None, endpoint_host: "h".into(), model: "m".into(), consent_json: "{}".into(), accounts: vec![ACC.into()] },
            T0,
        )
        .unwrap();
    let store = Arc::new(Mutex::new(st));
    let fake = Arc::new(FakeActions::new());
    let now = Arc::new(AtomicI64::new(T0));
    let n2 = now.clone();
    let exec = Executor::new(store.clone(), fake.clone(), Arc::new(move || n2.load(Ordering::SeqCst)), Arc::new(Mutex::new(HashSet::new())));
    Rig { store, fake, now, exec, conv }
}

fn scopes(folders: &[&str]) -> Vec<Scope> {
    folders.iter().map(|f| Scope { account: ACC.into(), folder: (*f).into() }).collect()
}

fn all_scopes() -> Vec<Scope> {
    scopes(&["INBOX", "Trash", "Archive", "Receipts"])
}

fn item_of(r: &Rig, folder: &str, uid: u32, to: &str) -> NewItem {
    use aichat::proposals::MailActionHost;
    let m = r.fake.lookup(ACC, folder, uid).unwrap().unwrap();
    NewItem { account: ACC.into(), folder: folder.into(), uid, uidvalidity: m.uidvalidity, sender: m.from, subject: m.subject, seen: m.seen, flagged: m.flagged, to_folder: to.into() }
}

fn fill(r: &Rig, n: u32) {
    for uid in 1..=n {
        r.fake.add("INBOX", uid, &format!("Receipt {uid}"), uid % 2 == 0);
    }
}

fn propose(r: &Rig, kind: Kind, params: Params, uids: &[u32]) -> i64 {
    let to = match kind {
        Kind::Move => params.dest_folder.clone().unwrap_or_default(),
        Kind::Trash => "Trash".into(),
        Kind::Archive => "Archive".into(),
        _ => String::new(),
    };
    let items: Vec<NewItem> = uids.iter().map(|u| item_of(r, "INBOX", *u, &to)).collect();
    let plan = build_plan(kind, params, items, 0, 0).unwrap();
    r.store
        .lock()
        .unwrap()
        .create_proposal(&NewProposal { conv_id: r.conv, kind, params: plan.params, title: plan.title, reason: "because".into(), warnings: plan.warnings, items: plan.items }, T0)
        .unwrap()
}

fn mv_params(dest: &str) -> Params {
    Params { dest_account: Some(ACC.into()), dest_folder: Some(dest.into()), ..Params::default() }
}

fn state(r: &Rig, id: i64) -> State {
    r.store.lock().unwrap().get_proposal(id).unwrap().unwrap().state
}

/// Approve with every row ticked (what the UI sends when nothing was unticked).
async fn approve_all_with(r: &Rig, id: i64, scopes: &[Scope], progress: &(dyn Fn(i64) + Send + Sync)) -> Result<ExecReport, ProposalError> {
    let idx: Vec<i64> = r.store.lock().unwrap().proposal_items(id).unwrap().iter().map(|i| i.idx).collect();
    r.exec.approve(id, &idx, scopes, progress).await
}

async fn approve_all(r: &Rig, id: i64, scopes: &[Scope]) -> Result<ExecReport, ProposalError> {
    approve_all_with(r, id, scopes, &noop_progress()).await
}

fn noop_progress() -> impl Fn(i64) + Send + Sync {
    |_| {}
}

// ------------------------------------------------------------------------------ state machine

#[test]
fn illegal_transitions_are_refused_and_change_nothing() {
    let r = rig();
    fill(&r, 2);
    let id = propose(&r, Kind::Trash, Params::default(), &[1]);
    let mut st = r.store.lock().unwrap();
    for from in State::ALL {
        for to in State::ALL {
            if from.can_become(to) {
                continue;
            }
            // wherever the proposal currently is, an illegal pair is refused
            let cur = st.get_proposal(id).unwrap().unwrap().state;
            let res = st.transition_proposal(id, from, to, T0, "");
            assert!(res.is_err(), "{from:?} -> {to:?}");
            assert_eq!(st.get_proposal(id).unwrap().unwrap().state, cur);
        }
    }
    // legal edge but wrong current state (CAS): Proposed row cannot go Executing -> Done
    assert_eq!(st.transition_proposal(id, State::Executing, State::Done, T0, ""), Err(ProposalError::Illegal { from: State::Proposed, to: State::Done }));
    assert_eq!(st.transition_proposal(999, State::Proposed, State::Approved, T0, ""), Err(ProposalError::NotFound));
    st.transition_proposal(id, State::Proposed, State::Approved, T0, "").unwrap();
    // second approval of the same row loses the race
    assert!(st.transition_proposal(id, State::Proposed, State::Approved, T0, "").is_err());
}

#[tokio::test]
async fn terminal_states_cannot_be_approved_or_rejected_again() {
    let r = rig();
    fill(&r, 3);
    let a = propose(&r, Kind::Trash, Params::default(), &[1]);
    r.exec.reject(a).unwrap();
    assert_eq!(state(&r, a), State::Rejected);
    assert!(matches!(approve_all(&r, a, &all_scopes()).await, Err(ProposalError::Illegal { from: State::Rejected, .. })));
    assert!(r.exec.reject(a).is_err());
    assert!(r.fake.mutations().is_empty(), "a rejected proposal never touches mail");

    let b = propose(&r, Kind::Trash, Params::default(), &[2]);
    approve_all(&r, b, &all_scopes()).await.unwrap();
    assert!(r.exec.reject(b).is_err());
    assert!(matches!(approve_all(&r, b, &all_scopes()).await, Err(ProposalError::Illegal { .. })));
    assert_eq!(r.fake.mutations().len(), 1, "approving twice executes once");
}

#[tokio::test]
async fn proposal_expires_after_ttl() {
    let r = rig();
    fill(&r, 1);
    let id = propose(&r, Kind::Archive, Params::default(), &[1]);
    r.now.store(T0 + TTL_SECS + 1, Ordering::SeqCst);
    assert_eq!(approve_all(&r, id, &all_scopes()).await, Err(ProposalError::Expired));
    assert_eq!(state(&r, id), State::Expired);
    assert!(r.fake.mutations().is_empty());
}

#[tokio::test]
async fn nothing_is_executed_until_approved_and_deselecting_everything_is_refused() {
    let r = rig();
    fill(&r, 3);
    let id = propose(&r, Kind::Move, mv_params("Receipts"), &[1, 2, 3]);
    assert_eq!(state(&r, id), State::Proposed);
    assert!(r.fake.mutations().is_empty());
    assert_eq!(r.exec.approve(id, &[], &all_scopes(), &noop_progress()).await, Err(ProposalError::NothingSelected));
    assert_eq!(state(&r, id), State::Proposed, "refusal leaves it proposed");
    assert!(r.fake.mutations().is_empty());
}

// ------------------------------------------------------------------------------ execution

#[tokio::test]
async fn approve_with_deselected_rows_moves_only_the_selected_ones() {
    let r = rig();
    fill(&r, 5);
    let id = propose(&r, Kind::Move, mv_params("Receipts"), &[1, 2, 3, 4, 5]);
    let rep = r.exec.approve(id, &[0, 2, 4], &all_scopes(), &noop_progress()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.failed, rep.skipped), (State::Done, 3, 0, 0));
    assert_eq!(r.fake.uids("INBOX"), vec![2, 4]);
    assert_eq!(r.fake.uids("Receipts").len(), 3);
    let items = r.store.lock().unwrap().proposal_items(id).unwrap();
    assert_eq!(items.iter().filter(|i| i.status == ItemStatus::Done).count(), 3);
    assert!(items.iter().filter(|i| !i.selected).all(|i| i.status == ItemStatus::Pending));
    assert!(rep.affected.contains(&(ACC.to_string(), "INBOX".to_string())) && rep.affected.contains(&(ACC.to_string(), "Receipts".to_string())));
}

#[tokio::test]
async fn revalidation_skips_stale_validity_moved_deleted_and_out_of_scope() {
    // stale UIDVALIDITY: the folder was reset after the proposal was made
    let r = rig();
    fill(&r, 3);
    let id = propose(&r, Kind::Trash, Params::default(), &[1, 2, 3]);
    r.fake.set_validity("INBOX", 8);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.skipped), (State::Failed, 0, 3));
    assert!(r.fake.mutations().is_empty(), "no server call for stale rows");
    let items = r.store.lock().unwrap().proposal_items(id).unwrap();
    assert!(items.iter().all(|i| i.status == ItemStatus::Skipped && i.error.contains("reset")));

    // one message was moved/deleted behind our back, the others still go
    let r = rig();
    fill(&r, 3);
    let id = propose(&r, Kind::Trash, Params::default(), &[1, 2, 3]);
    r.fake.remove("INBOX", 2);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.skipped), (State::PartiallyDone, 2, 1));
    assert_eq!(r.fake.uids("Trash").len(), 2);
    let items = r.store.lock().unwrap().proposal_items(id).unwrap();
    assert!(items[1].error.contains("no longer in this folder"));

    // out of scope: the executor's scope list does not contain the folder
    let r = rig();
    fill(&r, 2);
    let id = propose(&r, Kind::Trash, Params::default(), &[1, 2]);
    let rep = approve_all(&r, id, &scopes(&["Receipts"])).await.unwrap();
    assert_eq!((rep.state, rep.skipped), (State::Failed, 2));
    assert!(r.fake.mutations().is_empty());
    assert!(r.store.lock().unwrap().proposal_items(id).unwrap()[0].error.contains("outside the folders"));
}

#[tokio::test]
async fn trash_and_archive_resolve_the_role_folder_at_execution_time() {
    let r = rig();
    fill(&r, 2);
    let id = propose(&r, Kind::Archive, Params::default(), &[1]);
    // the archive folder disappears before approval
    r.fake.st.lock().unwrap().archive = None;
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(rep.state, State::Failed);
    assert!(r.store.lock().unwrap().proposal_items(id).unwrap()[0].error.contains("no Archive folder"));
    let id = propose(&r, Kind::Trash, Params::default(), &[2]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(rep.state, State::Done);
    assert_eq!(r.fake.uids("Trash").len(), 1, "trash is a move, never an expunge");
    assert_eq!(r.fake.uids("INBOX"), vec![1]);
}

#[tokio::test]
async fn big_proposals_run_in_chunks() {
    let r = rig();
    fill(&r, 450);
    let uids: Vec<u32> = (1..=450).collect();
    let id = propose(&r, Kind::Move, mv_params("Receipts"), &uids);
    let calls = Arc::new(Mutex::new(0usize));
    let c2 = calls.clone();
    let progress = move |_| *c2.lock().unwrap() += 1;
    let rep = approve_all_with(&r, id, &all_scopes(), &progress).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 450));
    assert_eq!(r.fake.st.lock().unwrap().move_calls, vec![EXEC_CHUNK, EXEC_CHUNK, 50]);
    assert!(*calls.lock().unwrap() >= 5, "progress after approval, each chunk and completion");
}

#[tokio::test]
async fn a_failing_chunk_stops_the_run_and_the_result_is_partial() {
    let r = rig();
    fill(&r, 450);
    let uids: Vec<u32> = (1..=450).collect();
    let id = propose(&r, Kind::Move, mv_params("Receipts"), &uids);
    r.fake.st.lock().unwrap().fail_move_call = Some(2);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.failed, rep.skipped), (State::PartiallyDone, 200, 200, 50));
    assert_eq!(r.fake.st.lock().unwrap().move_calls.len(), 2, "third chunk was not attempted");
    assert_eq!(r.fake.uids("INBOX").len(), 250);
    let items = r.store.lock().unwrap().proposal_items(id).unwrap();
    assert!(items[449].error.contains("Not attempted"));
    let counts = r.store.lock().unwrap().proposal_counts(id).unwrap();
    assert_eq!((counts.done, counts.failed, counts.skipped), (200, 200, 50));
}

#[tokio::test]
async fn per_item_server_refusals_make_it_partial_not_failed() {
    let r = rig();
    fill(&r, 4);
    r.fake.st.lock().unwrap().refuse_uids.insert(3);
    let id = propose(&r, Kind::Archive, Params::default(), &[1, 2, 3, 4]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.failed), (State::PartiallyDone, 3, 1));
    assert_eq!(r.fake.uids("INBOX"), vec![3]);
}

#[tokio::test]
async fn flags_apply_only_where_needed() {
    let r = rig();
    fill(&r, 4); // 2 and 4 are already read
    let id = propose(&r, Kind::Flag, Params { flag: Some(FlagOp::Read), ..Params::default() }, &[1, 2, 3, 4]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.noop), (State::Done, 2, 2));
    assert_eq!(r.fake.mutations(), vec!["flag INBOX seen=true [1, 3]".to_string()]);
    // everything already read: a no-op proposal still completes honestly
    let id = propose(&r, Kind::Flag, Params { flag: Some(FlagOp::Read), ..Params::default() }, &[1, 2]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.noop), (State::Done, 0, 2));
}

#[tokio::test]
async fn move_into_a_new_folder_creates_it_first_and_failure_to_create_moves_nothing() {
    let r = rig();
    fill(&r, 2);
    let p = Params { create_dest: true, ..mv_params("Tax 2026") };
    let id = propose(&r, Kind::Move, p.clone(), &[1, 2]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(rep.state, State::Done);
    assert!(rep.folders_changed);
    let m = r.fake.mutations();
    assert_eq!(m[0], "create Tax 2026");
    assert!(m[1].starts_with("move "));

    let r = rig();
    fill(&r, 2);
    r.fake.st.lock().unwrap().fail_create = true;
    let id = propose(&r, Kind::Move, p, &[1, 2]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(rep.state, State::Failed);
    assert!(r.fake.mutations().is_empty());
    assert_eq!(r.fake.uids("INBOX"), vec![1, 2]);
}

#[tokio::test]
async fn create_folder_proposal() {
    let r = rig();
    let params = Params { account: Some(ACC.into()), new_folder: Some("Receipts 2026".into()), ..Params::default() };
    let plan = build_plan(Kind::CreateFolder, params, vec![], 0, 0).unwrap();
    assert_eq!(plan.title, "Create folder \"Receipts 2026\"");
    let id = r.store.lock().unwrap().create_proposal(&NewProposal { conv_id: r.conv, kind: Kind::CreateFolder, params: plan.params, title: plan.title, reason: String::new(), warnings: vec![], items: vec![] }, T0).unwrap();
    assert!(r.fake.mutations().is_empty());
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.state, rep.ok, rep.folders_changed), (State::Done, 1, true));
    assert_eq!(r.fake.mutations(), vec!["create Receipts 2026".to_string()]);
    // creating a folder cannot be undone by this module (it never deletes folders)
    assert_eq!(r.exec.undo(id, &noop_progress()).await.map(|u| u.state), Err(ProposalError::NothingToUndo));
    assert_eq!(state(&r, id), State::Done);
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    let p = r.store.lock().unwrap().get_proposal(id).unwrap().unwrap();
    assert!(!undoable(&p, &log));
}

#[tokio::test]
async fn account_outside_scope_cannot_get_a_folder() {
    let r = rig();
    let params = Params { account: Some("other@example.org".into()), new_folder: Some("X".into()), ..Params::default() };
    let id = r.store.lock().unwrap().create_proposal(&NewProposal { conv_id: r.conv, kind: Kind::CreateFolder, params, title: "t".into(), reason: String::new(), warnings: vec![], items: vec![] }, T0).unwrap();
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(rep.state, State::Failed);
    assert!(r.fake.mutations().is_empty());
}

// ------------------------------------------------------------------------------ undo

#[tokio::test]
async fn undo_moves_everything_back_using_the_logged_new_uids() {
    let r = rig();
    fill(&r, 4);
    let id = propose(&r, Kind::Move, mv_params("Receipts"), &[1, 2, 3]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(r.fake.uids("INBOX"), vec![4]);
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    assert_eq!(log.len(), 3);
    assert!(log.iter().all(|l| l.dest_uid.is_some() && l.dest_validity == Some(7) && l.src_uid.is_some() && l.message_id.is_some()), "inverse data recorded");
    let rep = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((rep.state, rep.undone, rep.failed), (State::Undone, 3, 0));
    assert_eq!(r.fake.uids("Receipts"), Vec::<u32>::new());
    assert_eq!(r.fake.uids("INBOX").len(), 4);
    // the subjects are back where they were
    {
        let s = r.fake.st.lock().unwrap();
        assert!(s.msgs.iter().filter(|m| m.folder == "INBOX").any(|m| m.subject == "Receipt 1"));
    }
    assert_eq!(state(&r, id), State::Undone);
    // a second undo is illegal
    assert!(matches!(r.exec.undo(id, &noop_progress()).await, Err(ProposalError::Illegal { from: State::Undone, .. })));
}

#[tokio::test]
async fn undo_falls_back_to_message_id_when_the_new_uid_is_unknown() {
    let r = rig();
    fill(&r, 2);
    r.fake.st.lock().unwrap().hide_new_uid = true;
    let id = propose(&r, Kind::Trash, Params::default(), &[1, 2]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    assert!(r.store.lock().unwrap().list_log(id).unwrap().iter().all(|l| l.dest_uid.is_none() && l.message_id.is_some()));
    let rep = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((rep.state, rep.undone), (State::Undone, 2));
    assert_eq!(r.fake.uids("INBOX").len(), 2);
}

#[tokio::test]
async fn undo_after_partial_execution_reverses_only_what_was_done() {
    let r = rig();
    fill(&r, 450);
    let uids: Vec<u32> = (1..=450).collect();
    let id = propose(&r, Kind::Move, mv_params("Receipts"), &uids);
    r.fake.st.lock().unwrap().fail_move_call = Some(2);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(rep.state, State::PartiallyDone);
    let rep = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((rep.state, rep.undone, rep.failed), (State::Undone, 200, 0));
    assert_eq!(r.fake.uids("INBOX").len(), 450);
}

#[tokio::test]
async fn partial_undo_failure_is_reported_and_can_be_retried() {
    let r = rig();
    fill(&r, 3);
    let id = propose(&r, Kind::Trash, Params::default(), &[1, 2, 3]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    // the user already removed one of the trashed messages elsewhere, and the server refuses another
    let (gone, refused) = {
        let s = r.fake.st.lock().unwrap();
        let t: Vec<u32> = s.msgs.iter().filter(|m| m.folder == "Trash").map(|m| m.uid).collect();
        (t[0], t[1])
    };
    r.fake.remove("Trash", gone);
    r.fake.st.lock().unwrap().refuse_uids.insert(refused);
    let rep = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((rep.undone, rep.failed), (1, 2));
    assert_eq!(rep.state, State::Done, "not Undone while something could not be reversed");
    assert!(!rep.errors.is_empty() && rep.errors.len() <= 5);
    let c = r.store.lock().unwrap().proposal_counts(id).unwrap();
    assert_eq!((c.undone, c.undo_failed), (1, 2));
    // the server relents; the retry handles the one that can still be moved, the vanished one keeps failing
    r.fake.st.lock().unwrap().refuse_uids.clear();
    let rep = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((rep.undone, rep.failed), (1, 1));
    assert_eq!(rep.state, State::Done);
    assert_eq!(r.fake.uids("INBOX").len(), 2);
}

#[tokio::test]
async fn undo_restores_flags_and_skips_unchanged_ones() {
    let r = rig();
    fill(&r, 4); // 2 and 4 read
    let id = propose(&r, Kind::Flag, Params { flag: Some(FlagOp::Read), ..Params::default() }, &[1, 2, 3]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    assert!(r.fake.st.lock().unwrap().msgs.iter().filter(|m| m.uid <= 3).all(|m| m.seen));
    let rep = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((rep.state, rep.undone), (State::Undone, 2));
    let s = r.fake.st.lock().unwrap();
    let seen: Vec<(u32, bool)> = s.msgs.iter().map(|m| (m.uid, m.seen)).collect();
    assert!(seen.contains(&(1, false)) && seen.contains(&(2, true)) && seen.contains(&(3, false)) && seen.contains(&(4, true)), "{seen:?}");
}

#[tokio::test]
async fn undo_is_refused_for_proposals_that_never_ran() {
    let r = rig();
    fill(&r, 1);
    let id = propose(&r, Kind::Trash, Params::default(), &[1]);
    assert!(matches!(r.exec.undo(id, &noop_progress()).await, Err(ProposalError::Illegal { from: State::Proposed, .. })));
    r.exec.reject(id).unwrap();
    assert!(r.exec.undo(id, &noop_progress()).await.is_err());
    assert!(r.fake.mutations().is_empty());
}

// ------------------------------------------------------------------------------ review fixes

#[tokio::test]
async fn approve_runs_only_the_named_rows_and_ignores_junk_indexes() {
    let r = rig();
    fill(&r, 4);
    let id = propose(&r, Kind::Trash, Params::default(), &[1, 2, 3, 4]);
    // too many indexes: refused before anything happens
    let many: Vec<i64> = (0..MAX_ITEMS as i64 + 1).collect();
    assert_eq!(r.exec.approve(id, &many, &all_scopes(), &noop_progress()).await, Err(ProposalError::TooMany));
    assert_eq!(state(&r, id), State::Proposed);
    // indexes that do not exist select nothing
    assert_eq!(r.exec.approve(id, &[77, -1], &all_scopes(), &noop_progress()).await, Err(ProposalError::NothingSelected));
    assert!(r.fake.mutations().is_empty());
    // only row 2 is named -> only that message moves
    let rep = r.exec.approve(id, &[2, 2], &all_scopes(), &noop_progress()).await.unwrap();
    assert_eq!((rep.state, rep.ok), (State::Done, 1));
    assert_eq!(r.fake.uids("INBOX"), vec![1, 2, 4]);
}

#[tokio::test]
async fn a_lost_reply_is_recorded_as_unverified_and_undo_can_still_put_the_message_back() {
    let r = rig();
    fill(&r, 3);
    {
        let mut s = r.fake.st.lock().unwrap();
        s.lose_reply_on_move_call = Some(1); // the server executes the MOVE, the answer never arrives
        s.verify_can_tell = false; // and the follow-up check cannot tell either
    }
    let id = propose(&r, Kind::Trash, Params::default(), &[1, 2, 3]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.ok, rep.failed, rep.unverified), (0, 0, 3));
    assert_eq!(rep.state, State::PartiallyDone, "not Failed: the messages may be moved, so Undo must stay available");
    assert_eq!(r.fake.uids("INBOX"), Vec::<u32>::new(), "the server really did move them");
    let items = r.store.lock().unwrap().proposal_items(id).unwrap();
    assert!(items.iter().all(|i| i.status == ItemStatus::Unknown && i.error.contains("Not confirmed")));
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    assert_eq!(log.len(), 3);
    assert!(log.iter().all(|l| l.outcome == "ok-unverified" && l.message_id.is_some() && l.dest_uid.is_none() && l.src_uid.is_some()), "{log:?}");
    assert_eq!(r.store.lock().unwrap().proposal_counts(id).unwrap().unknown, 3);
    let p = r.store.lock().unwrap().get_proposal(id).unwrap().unwrap();
    assert!(undoable(&p, &log));
    // Undo finds them in Trash by Message-ID
    let up = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((up.state, up.undone, up.failed), (State::Undone, 3, 0));
    assert_eq!(r.fake.uids("INBOX").len(), 3);
}

#[tokio::test]
async fn undos_own_lost_reply_is_reported_not_claimed_as_undone() {
    let r = rig();
    fill(&r, 1);
    let id = propose(&r, Kind::Trash, Params::default(), &[1]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    {
        let mut s = r.fake.st.lock().unwrap();
        s.lose_reply_on_move_call = Some(2); // call 1 was the approval
        s.verify_can_tell = false;
    }
    let up = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((up.undone, up.failed), (0, 1));
    assert!(up.errors[0].contains("Not confirmed"), "{:?}", up.errors);
    assert_eq!(up.state, State::Done, "still undoable: a retry can look again");
}

#[tokio::test]
async fn flag_noop_and_old_value_come_from_the_server_not_the_cache() {
    let r = rig();
    fill(&r, 2); // server: uid 1 unread, uid 2 read
    // the cache is stale: it says uid 1 is already read and uid 2 is unread
    r.fake.st.lock().unwrap().stale_seen.extend([(("INBOX".to_string(), 1), true), (("INBOX".to_string(), 2), false)]);
    let id = propose(&r, Kind::Flag, Params { flag: Some(FlagOp::Read), ..Params::default() }, &[1, 2]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!((rep.ok, rep.noop), (1, 1), "uid 1 really changed, uid 2 really was already read");
    assert_eq!(r.fake.mutations(), vec!["flag INBOX seen=true [1]".to_string()]);
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!((log[0].src_uid, log[0].flag_old, log[0].flag_new), (Some(1), Some(false), Some(true)), "the server's old value is logged");
    let items = r.store.lock().unwrap().proposal_items(id).unwrap();
    assert_eq!((items[0].status, items[1].status), (ItemStatus::Done, ItemStatus::Noop));
}

#[tokio::test]
async fn create_dest_checks_scope_before_creating_and_logs_an_explicit_outcome() {
    // source folder not in the conversation's scope: nothing is created
    let r = rig();
    fill(&r, 2);
    let p = Params { create_dest: true, ..mv_params("Tax 2026") };
    let id = propose(&r, Kind::Move, p.clone(), &[1, 2]);
    let rep = approve_all(&r, id, &scopes(&["Receipts"])).await.unwrap();
    assert_eq!((rep.state, rep.skipped, rep.folders_changed), (State::Failed, 2, false));
    assert!(r.fake.mutations().is_empty(), "the folder must not be created: {:?}", r.fake.mutations());
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    let c = log.iter().find(|l| l.op == "create_folder").unwrap();
    assert_eq!(c.outcome, "skipped");

    // destination account is not one of the conversation's accounts: nothing is created either
    let r = rig();
    fill(&r, 1);
    let p2 = Params { dest_account: Some("other@example.org".into()), ..p.clone() };
    let id = propose(&r, Kind::Move, p2, &[1]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    assert!(r.fake.mutations().is_empty());

    // created: "ok"
    let r = rig();
    fill(&r, 1);
    let id = propose(&r, Kind::Move, p.clone(), &[1]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    assert_eq!(log.iter().find(|l| l.op == "create_folder").unwrap().outcome, "ok");

    // already there (made by someone else since the proposal): "exists", not created again
    let r = rig();
    fill(&r, 1);
    let id = propose(&r, Kind::Move, Params { create_dest: true, ..mv_params("Receipts") }, &[1]);
    let rep = approve_all(&r, id, &all_scopes()).await.unwrap();
    assert!(!rep.folders_changed);
    assert!(r.fake.mutations().iter().all(|m| !m.starts_with("create")));
    let log = r.store.lock().unwrap().list_log(id).unwrap();
    let c = log.iter().find(|l| l.op == "create_folder").unwrap();
    assert_eq!(c.outcome, "exists");
    assert!(log.iter().all(|l| !l.outcome.is_empty()), "no blank outcomes: {log:?}");

    // a create_folder proposal for a folder that exists: "exists" as well
    let r = rig();
    let params = Params { account: Some(ACC.into()), new_folder: Some("Receipts".into()), ..Params::default() };
    let id = r.store.lock().unwrap().create_proposal(&NewProposal { conv_id: r.conv, kind: Kind::CreateFolder, params, title: "t".into(), reason: String::new(), warnings: vec![], items: vec![] }, T0).unwrap();
    approve_all(&r, id, &all_scopes()).await.unwrap();
    assert_eq!(r.store.lock().unwrap().list_log(id).unwrap()[0].outcome, "exists");
}

#[tokio::test]
async fn flag_undo_skips_rows_without_a_uid_and_restores_the_rest() {
    let r = rig();
    fill(&r, 3);
    let id = propose(&r, Kind::Flag, Params { flag: Some(FlagOp::Flagged), ..Params::default() }, &[1, 2, 3]);
    approve_all(&r, id, &all_scopes()).await.unwrap();
    // a damaged log row (no src_uid) sits in the same group as the good ones
    let mut bad = blank_log(id);
    bad.op = "flag".into();
    bad.flag = "flagged".into();
    bad.flag_old = Some(false);
    bad.flag_new = Some(true);
    bad.src_uid = None;
    bad.item_idx = 0;
    r.store.lock().unwrap().add_log(&bad, T0).unwrap();
    let up = r.exec.undo(id, &noop_progress()).await.unwrap();
    assert_eq!((up.undone, up.failed), (3, 1), "the three good rows are restored, the bad one is reported");
    assert!(up.errors.iter().any(|e| e.contains("not recorded")), "{:?}", up.errors);
    assert!(r.fake.st.lock().unwrap().msgs.iter().all(|m| !m.flagged));
    assert_eq!(up.state, State::Done, "not Undone while a row could not be reversed");
}

// ------------------------------------------------------------------------------ misc

#[test]
fn interrupted_executions_are_settled_on_startup() {
    let r = rig();
    fill(&r, 2);
    let a = propose(&r, Kind::Trash, Params::default(), &[1]);
    let b = propose(&r, Kind::Trash, Params::default(), &[2]);
    let mut st = r.store.lock().unwrap();
    for id in [a, b] {
        st.transition_proposal(id, State::Proposed, State::Approved, T0, "").unwrap();
        st.transition_proposal(id, State::Approved, State::Executing, T0, "").unwrap();
    }
    let mut row = LogRowProbe::ok(a);
    row.insert(&mut st);
    assert_eq!(st.recover_interrupted(T0 + 5).unwrap(), 2);
    assert_eq!(st.get_proposal(a).unwrap().unwrap().state, State::PartiallyDone);
    assert_eq!(st.get_proposal(b).unwrap().unwrap().state, State::Failed);
    assert!(st.get_proposal(b).unwrap().unwrap().error.contains("closed"));
}

/// Build an ok log row through the public API (the type has a private constructor).
struct LogRowProbe(i64);
impl LogRowProbe {
    fn ok(id: i64) -> Self {
        LogRowProbe(id)
    }
    fn insert(&mut self, st: &mut Store) {
        let mut row = st.list_log(self.0).unwrap().pop().unwrap_or_else(|| blank_log(self.0));
        row.outcome = "ok".into();
        st.add_log(&row, T0).unwrap();
    }
}
fn blank_log(proposal_id: i64) -> LogRow {
    LogRow {
        id: 0,
        proposal_id,
        item_idx: 0,
        ts: 0,
        op: "move".into(),
        account: ACC.into(),
        src_folder: "INBOX".into(),
        src_uid: Some(1),
        src_validity: Some(7),
        dest_folder: "Trash".into(),
        dest_uid: Some(1),
        dest_validity: Some(7),
        message_id: None,
        flag: String::new(),
        flag_old: None,
        flag_new: None,
        outcome: "ok".into(),
        error: String::new(),
        undone: 0,
        undo_error: String::new(),
    }
}

#[test]
fn deleting_a_conversation_removes_its_proposals_and_log() {
    let r = rig();
    fill(&r, 1);
    let id = propose(&r, Kind::Trash, Params::default(), &[1]);
    let mut st = r.store.lock().unwrap();
    st.add_log(&blank_log(id), T0).unwrap();
    st.delete_conversation(r.conv).unwrap();
    assert!(st.get_proposal(id).unwrap().is_none());
    assert!(st.list_log(id).unwrap().is_empty());
    assert!(st.proposal_items(id).unwrap().is_empty());
}

#[test]
fn caps_are_enforced_on_the_plan() {
    let items = |n: usize| -> Vec<NewItem> {
        (0..n).map(|i| NewItem { account: ACC.into(), folder: "INBOX".into(), uid: i as u32 + 1, uidvalidity: 7, sender: "s".into(), subject: "x".into(), seen: true, flagged: false, to_folder: "T".into() }).collect()
    };
    assert!(build_plan(Kind::Trash, Params::default(), items(MAX_ITEMS), 0, 0).is_ok());
    assert!(build_plan(Kind::Trash, Params::default(), items(MAX_ITEMS + 1), 0, 0).is_err());
}
