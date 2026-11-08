//! Rule runner against an in-memory fake mailbox: cursor semantics (each message once, reset on UIDVALIDITY
//! change), kill switch, Junk/Trash skipping, claim and loop guards, per-run undo, dry run. No network, no
//! real mailbox, no Keychain.
mod support;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use aichat::proposals::{Executor, State};
use aichat::rules::*;
use aichat::store::Store;
use support::actions::{FakeActions, ACC};

const T0: i64 = 1_800_000_000;

/// Read side of the runner, answered from the same fake the executor mutates.
struct FakeMail(Arc<FakeActions>);

impl FakeMail {
    fn facts(m: &support::actions::FMsg) -> MsgFacts {
        MsgFacts { from: m.from.clone(), to: ACC.into(), subject: m.subject.clone(), folder: m.folder.clone(), ..MsgFacts::default() }
    }
}

impl RuleMail for FakeMail {
    fn folders(&self, account: &str) -> Result<Vec<String>, String> {
        let mut v: Vec<String> = self.0.st.lock().unwrap().validity.keys().filter(|(a, _)| a == account).map(|(_, f)| f.clone()).collect();
        v.sort();
        Ok(v)
    }
    fn folder_role(&self, _account: &str, folder: &str) -> Result<Option<String>, String> {
        // a localised Inbox: the role is `inbox` although the name is not INBOX
        Ok(Some(if folder == "Postfach" { "inbox".to_string() } else { folder.to_ascii_lowercase() }))
    }
    fn validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        Ok(self.0.st.lock().unwrap().validity.get(&(account.into(), folder.into())).copied())
    }
    fn max_uid(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        Ok(self.0.st.lock().unwrap().msgs.iter().filter(|m| m.account == account && m.folder == folder).map(|m| m.uid).max())
    }
    fn folder_exists(&self, account: &str, folder: &str) -> Result<bool, String> {
        Ok(self.0.st.lock().unwrap().validity.contains_key(&(account.into(), folder.into())))
    }
    fn after(&self, account: &str, folder: &str, after_uid: u32, limit: u32) -> Result<Vec<RuleMsg>, String> {
        let s = self.0.st.lock().unwrap();
        let v = s.validity.get(&(account.into(), folder.into())).copied().unwrap_or(0);
        let mut out: Vec<RuleMsg> = s
            .msgs
            .iter()
            .filter(|m| m.account == account && m.folder == folder && m.uid > after_uid)
            .map(|m| RuleMsg { account: m.account.clone(), folder: m.folder.clone(), uid: m.uid, uidvalidity: v, message_id: m.message_id.clone(), facts: Self::facts(m), seen: m.seen, flagged: m.flagged, date: 100, size: 10 })
            .collect();
        out.sort_by_key(|m| m.uid);
        out.truncate(limit as usize);
        Ok(out)
    }
    fn recent(&self, account: &str, folder: &str, limit: u32) -> Result<Vec<RuleMsg>, String> {
        let mut v = self.after(account, folder, 0, u32::MAX)?;
        v.reverse();
        v.truncate(limit as usize);
        Ok(v)
    }
}

struct Rig {
    store: Arc<Mutex<Store>>,
    fake: Arc<FakeActions>,
    runner: RuleRunner,
    busy: aichat::proposals::Busy,
}

fn rig() -> Rig {
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let fake = Arc::new(FakeActions::new());
    // extra folders: a Junk folder and two plain ones for the ping-pong test
    for f in ["Junk", "X", "Y"] {
        fake.set_validity(f, 7);
    }
    let busy: aichat::proposals::Busy = Arc::new(Mutex::new(HashSet::new()));
    let exec = Executor::new(store.clone(), fake.clone(), Arc::new(|| T0), busy.clone());
    let runner = RuleRunner::new(store.clone(), Arc::new(FakeMail(fake.clone())), exec);
    Rig { store, fake, runner, busy }
}

fn cond(field: Field, op: Op, value: &str) -> Condition {
    Condition { field, op, value: value.into() }
}

fn receipts_rule() -> RuleSpec {
    RuleSpec { v: 1, mode: MatchMode::All, conditions: vec![cond(Field::Subject, Op::Contains, "receipt")], actions: vec![Action::MoveTo { folder: "Receipts".into() }, Action::MarkRead] }
}

fn add_rule(r: &Rig, name: &str, spec: RuleSpec, enabled: bool) -> i64 {
    let id = r.store.lock().unwrap().create_rule(&NewRule { account: ACC.into(), name: name.into(), spec, origin: "user", conv_id: None, reason: String::new(), enabled }, T0).unwrap();
    if enabled {
        r.runner.baseline(id).unwrap();
    }
    id
}

fn enable(r: &Rig, id: i64) {
    r.store.lock().unwrap().set_rule_enabled(id, true, T0).unwrap();
    r.runner.baseline(id).unwrap();
}

async fn run(r: &Rig, folder: &str) -> Vec<RunReport> {
    r.runner.run_folder(ACC, folder, T0).await.unwrap()
}

#[tokio::test]
async fn each_message_is_processed_once_and_only_new_mail_counts() {
    let r = rig();
    r.fake.add("INBOX", 1, "Old receipt", false); // before the rule is switched on: never touched
    let id = add_rule(&r, "Receipts", receipts_rule(), false);
    enable(&r, id);
    assert!(run(&r, "INBOX").await.is_empty());
    assert!(r.fake.mutations().is_empty(), "existing mail is not touched by enabling");

    r.fake.add("INBOX", 2, "Your receipt", false);
    r.fake.add("INBOX", 3, "Lunch?", false);
    r.fake.add("INBOX", 4, "Another RECEIPT", false);
    let reps = run(&r, "INBOX").await;
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].matched, 2);
    assert!(reps[0].summary.contains("2 moved to Receipts") && reps[0].summary.contains("2 marked read"), "{}", reps[0].summary);
    assert_eq!(r.fake.uids("INBOX"), vec![1, 3]);
    assert_eq!(r.fake.uids("Receipts").len(), 2);
    let n = r.fake.mutations().len();

    // Running again over the same folder does nothing: the cursor moved past 4.
    assert!(run(&r, "INBOX").await.is_empty());
    assert!(run(&r, "INBOX").await.is_empty());
    assert_eq!(r.fake.mutations().len(), n);

    // A later arrival is processed, and only it.
    r.fake.add("INBOX", 5, "receipt #5", false);
    let reps = run(&r, "INBOX").await;
    assert_eq!(reps[0].matched, 1);
    assert_eq!(r.fake.uids("INBOX"), vec![1, 3]);

    // The moved messages show up as new mail in Receipts; the rule (INBOX only) ignores them.
    assert!(run(&r, "Receipts").await.is_empty());
    let cur = r.store.lock().unwrap().get_cursor(id, ACC, "INBOX").unwrap().unwrap();
    assert!(cur.last_uid >= 5);
}

#[tokio::test]
async fn cursor_resets_when_uidvalidity_changes_and_does_not_replay_the_folder() {
    let r = rig();
    let id = add_rule(&r, "Receipts", receipts_rule(), true);
    r.fake.add("INBOX", 1, "receipt A", false);
    assert_eq!(run(&r, "INBOX").await.len(), 1);
    let before = r.fake.mutations().len();

    // The server re-created the folder: every UID is new. Mail that is already there is NOT replayed.
    r.fake.set_validity("INBOX", 99);
    r.fake.add("INBOX", 1, "receipt B (new numbering)", false);
    r.fake.add("INBOX", 2, "receipt C", false);
    assert!(run(&r, "INBOX").await.is_empty(), "first pass after a reset only re-baselines");
    assert_eq!(r.fake.mutations().len(), before);
    let cur = r.store.lock().unwrap().get_cursor(id, ACC, "INBOX").unwrap().unwrap();
    assert_eq!((cur.uidvalidity, cur.last_uid), (99, 2));

    // New mail after the reset is handled normally.
    r.fake.add("INBOX", 3, "receipt D", false);
    let reps = run(&r, "INBOX").await;
    assert_eq!(reps[0].matched, 1);
}

#[tokio::test]
async fn kill_switch_stops_all_rules_and_paused_mail_is_not_processed_later() {
    let r = rig();
    add_rule(&r, "Receipts", receipts_rule(), true);
    r.store.lock().unwrap().set_rules_paused(true).unwrap();
    r.fake.add("INBOX", 1, "receipt during pause", false);
    assert!(run(&r, "INBOX").await.is_empty());
    assert!(r.fake.mutations().is_empty());
    r.store.lock().unwrap().set_rules_paused(false).unwrap();
    assert!(run(&r, "INBOX").await.is_empty(), "mail from the pause is not acted on after resuming");
    r.fake.add("INBOX", 2, "receipt after resume", false);
    assert_eq!(run(&r, "INBOX").await[0].matched, 1);
    assert_eq!(r.fake.uids("INBOX"), vec![1]);
}

#[tokio::test]
async fn disabled_rules_and_junk_or_trash_folders_are_never_touched() {
    let r = rig();
    add_rule(&r, "off", receipts_rule(), false);
    let mut spec = receipts_rule();
    spec.conditions.push(cond(Field::Folder, Op::Glob, "J*")); // covers Junk
    let id = add_rule(&r, "wide", spec, true);
    r.fake.add("INBOX", 1, "receipt", false);
    r.fake.add("Junk", 1, "receipt", false);
    r.fake.add("Trash", 1, "receipt", false);
    // baseline was taken before the mail existed -> also seed the cursors low for Junk/Trash to prove they are skipped by role
    assert!(run(&r, "Junk").await.is_empty());
    assert!(run(&r, "Trash").await.is_empty());
    assert!(r.fake.mutations().is_empty());
    assert!(r.store.lock().unwrap().get_cursor(id, ACC, "Junk").unwrap().is_none(), "no cursor is even created for Junk/Trash");
    // the disabled rule does nothing in INBOX; the enabled one is limited to its folder glob
    assert!(run(&r, "INBOX").await.is_empty());
    assert!(r.fake.mutations().is_empty());
}

#[tokio::test]
async fn first_enabled_rule_wins_a_message_for_moving_but_flags_still_apply() {
    let r = rig();
    let a = add_rule(&r, "A", RuleSpec { v: 1, mode: MatchMode::All, conditions: vec![cond(Field::Subject, Op::Contains, "invoice")], actions: vec![Action::MoveTo { folder: "Receipts".into() }] }, true);
    let b = add_rule(&r, "B", RuleSpec { v: 1, mode: MatchMode::All, conditions: vec![cond(Field::Subject, Op::Contains, "invoice")], actions: vec![Action::Archive] }, true);
    let c = add_rule(&r, "C", RuleSpec { v: 1, mode: MatchMode::All, conditions: vec![cond(Field::Subject, Op::Contains, "invoice")], actions: vec![Action::Flag] }, true);
    r.fake.add("INBOX", 1, "invoice 1", false);
    let reps = run(&r, "INBOX").await;
    let by = |id: i64| reps.iter().find(|x| x.rule_id == id);
    assert!(by(a).is_some() && by(b).is_none(), "B lost the claim");
    assert!(by(c).is_some(), "a flag-only rule is not blocked");
    assert_eq!(r.fake.uids("Receipts").len(), 1);
    assert!(r.fake.st.lock().unwrap().msgs.iter().any(|m| m.folder == "Receipts" && m.flagged), "flagged before it moved");
    assert!(r.fake.uids("Archive").is_empty());
}

#[tokio::test]
async fn two_rules_cannot_ping_pong_a_message_between_folders() {
    let r = rig();
    let there = |from: &str, to: &str| RuleSpec {
        v: 1,
        mode: MatchMode::All,
        conditions: vec![cond(Field::Subject, Op::Contains, "ball"), cond(Field::Folder, Op::Equals, from)],
        actions: vec![Action::MoveTo { folder: to.into() }],
    };
    add_rule(&r, "X to Y", there("X", "Y"), true);
    add_rule(&r, "Y to X", there("Y", "X"), true);
    r.fake.add("X", 1, "the ball", false);
    assert_eq!(run(&r, "X").await.len(), 1);
    assert_eq!(r.fake.uids("Y").len(), 1);
    // The moved message arrives in Y; the second rule must leave it alone.
    assert!(run(&r, "Y").await.is_empty());
    assert_eq!(r.fake.uids("Y").len(), 1);
    assert!(r.fake.uids("X").is_empty());
    assert_eq!(r.fake.mutations().len(), 1, "exactly one move happened");
}

#[tokio::test]
async fn a_run_can_be_undone_as_a_whole() {
    let r = rig();
    let id = add_rule(&r, "Receipts", receipts_rule(), true);
    r.fake.add("INBOX", 1, "receipt one", false);
    r.fake.add("INBOX", 2, "receipt two", true);
    r.fake.add("INBOX", 3, "hello", false);
    let rep = run(&r, "INBOX").await.remove(0);
    assert_eq!(rep.proposal_ids.len(), 2, "one logged step for the flag change, one for the move");
    assert_eq!(r.fake.uids("INBOX"), vec![3]);
    let seen_after = |uid: u32| r.fake.st.lock().unwrap().msgs.iter().find(|m| m.folder == "Receipts" && m.message_id.as_deref() == Some(&format!("INBOX-{uid}@x"))).map(|m| m.seen);
    assert_eq!(seen_after(1), Some(true), "marked read by the rule");

    let un = r.runner.undo_run(rep.run_id).await.unwrap();
    assert_eq!(un.failed, 0, "{:?}", un.errors);
    assert!(un.undone >= 3);
    let mut back = r.fake.uids("INBOX");
    back.sort_unstable();
    assert_eq!(back.len(), 3, "both messages are back in the Inbox");
    assert!(r.fake.uids("Receipts").is_empty());
    {
        let st = r.fake.st.lock().unwrap();
        let one = st.msgs.iter().find(|m| m.message_id.as_deref() == Some("INBOX-1@x")).unwrap();
        let two = st.msgs.iter().find(|m| m.message_id.as_deref() == Some("INBOX-2@x")).unwrap();
        assert!(!one.seen, "unread again, as before the rule");
        assert!(two.seen, "was already read, stays read");
    }

    // Proposals of the run are Undone; a second undo is refused; the log shows the run.
    let states: Vec<State> = {
        let mut s = r.store.lock().unwrap();
        rep.proposal_ids.iter().map(|p| s.get_proposal(*p).unwrap().unwrap().state).collect()
    };
    assert!(states.iter().all(|s| *s == State::Undone || *s == State::Done), "{states:?}");
    assert!(r.runner.undo_run(rep.run_id).await.is_err());
    assert_eq!(r.store.lock().unwrap().list_rule_runs(10).unwrap().len(), 1);
    // After the undo the loop guard no longer shields the messages (the user moved them back on purpose).
    assert!(!r.store.lock().unwrap().rule_moved_recently(ACC, "INBOX-1@x", T0 - 10).unwrap());
    let _ = id;
}

#[tokio::test]
async fn rule_runs_do_not_appear_in_the_chat_activity_log() {
    let r = rig();
    add_rule(&r, "Receipts", receipts_rule(), true);
    r.fake.add("INBOX", 1, "receipt", false);
    run(&r, "INBOX").await;
    let mut st = r.store.lock().unwrap();
    assert!(st.list_proposals(None, 50).unwrap().is_empty());
    assert!(st.list_conversations().unwrap().is_empty());
    assert_eq!(st.list_proposals(Some(0), 50).unwrap().len(), 2);
}

#[tokio::test]
async fn missing_destination_folder_moves_nothing() {
    let r = rig();
    add_rule(&r, "Ghost", RuleSpec { v: 1, mode: MatchMode::All, conditions: vec![cond(Field::Subject, Op::Contains, "receipt")], actions: vec![Action::MoveTo { folder: "Nope".into() }] }, true);
    r.fake.add("INBOX", 1, "receipt", false);
    let reps = run(&r, "INBOX").await;
    assert!(reps[0].summary.contains("does not exist"), "{}", reps[0].summary);
    assert!(r.fake.mutations().is_empty());
    assert_eq!(r.fake.uids("INBOX"), vec![1]);
}

#[tokio::test]
async fn dry_run_counts_and_samples_without_touching_anything() {
    let r = rig();
    for uid in 1..=30 {
        r.fake.add("INBOX", uid, &format!("receipt {uid}"), false);
    }
    r.fake.add("INBOX", 31, "hello", false);
    r.fake.add("Junk", 1, "receipt in junk", false);
    let d = r.runner.dry_run(ACC, &receipts_rule()).unwrap();
    assert_eq!((d.scanned, d.matched), (31, 30), "INBOX only; Junk/Trash are ignored");
    assert_eq!(d.sample.len(), DRY_RUN_SAMPLE);
    assert!(d.sample.iter().all(|s| s.folder == "INBOX" && s.subject.starts_with("receipt")));
    assert!(r.fake.mutations().is_empty());
    assert!(r.store.lock().unwrap().list_rule_runs(10).unwrap().is_empty());
}

#[tokio::test]
async fn bad_stored_specs_never_run() {
    let r = rig();
    let id = add_rule(&r, "bad", receipts_rule(), true);
    r.store.lock().unwrap().raw().execute("UPDATE rules SET spec_json='{\"v\":1,\"conditions\":[{\"field\":\"subject\",\"op\":\"regex\",\"value\":\".*\"}],\"actions\":[{\"type\":\"delete\"}]}'", []).unwrap();
    r.fake.add("INBOX", 1, "receipt", false);
    assert!(run(&r, "INBOX").await.is_empty());
    assert!(r.fake.mutations().is_empty());
    let _ = id;
}

#[tokio::test]
async fn ping_pong_is_stopped_even_without_a_message_id() {
    let r = rig();
    let there = |from: &str, to: &str| RuleSpec {
        v: 1,
        mode: MatchMode::All,
        conditions: vec![cond(Field::Subject, Op::Contains, "ball"), cond(Field::Folder, Op::Equals, from)],
        actions: vec![Action::MoveTo { folder: to.into() }],
    };
    add_rule(&r, "X to Y", there("X", "Y"), true);
    add_rule(&r, "Y to X", there("Y", "X"), true);
    r.fake.add("X", 1, "the ball", false);
    r.fake.st.lock().unwrap().msgs[0].message_id = None;
    assert_eq!(run(&r, "X").await.len(), 1);
    assert_eq!(r.fake.uids("Y").len(), 1);
    assert!(run(&r, "Y").await.is_empty(), "the fingerprint guard recognises the moved message");
    assert_eq!(r.fake.mutations().len(), 1, "exactly one move happened");
}

#[tokio::test]
async fn busy_executor_does_not_lose_the_mail_it_is_retried_next_pass() {
    let r = rig();
    add_rule(&r, "Receipts", receipts_rule(), true);
    r.fake.add("INBOX", 1, "receipt A", false);
    // every proposal id the runner will use is "already being worked on"
    r.busy.lock().unwrap().extend(1..=50);
    let reps = run(&r, "INBOX").await;
    assert_eq!(reps.len(), 1);
    assert!(reps[0].summary.contains("another change was running"), "{}", reps[0].summary);
    assert_eq!(r.fake.uids("INBOX"), vec![1], "nothing moved yet");
    r.busy.lock().unwrap().clear();
    let reps = run(&r, "INBOX").await;
    assert_eq!(reps.len(), 1, "the same message is looked at again");
    assert_eq!(reps[0].moved_uids, vec![1]);
    assert!(r.fake.uids("INBOX").is_empty() && r.fake.uids("Receipts").len() == 1);
    assert!(run(&r, "INBOX").await.is_empty(), "and only once");
}

#[tokio::test]
async fn ai_rules_may_not_move_mail_to_the_inbox_role_and_edits_switch_a_rule_off() {
    let r = rig();
    r.fake.set_validity("Postfach", 7);
    let spec = RuleSpec { v: 1, mode: MatchMode::All, conditions: vec![cond(Field::Subject, Op::Contains, "bounce")], actions: vec![Action::MoveTo { folder: "Postfach".into() }] };
    let mk = |origin: &'static str| r.store.lock().unwrap().create_rule(&NewRule { account: ACC.into(), name: origin.into(), spec: spec.clone(), origin, conv_id: None, reason: String::new(), enabled: true }, T0).unwrap();
    let ai = mk("ai");
    r.runner.baseline(ai).unwrap();
    r.fake.add("INBOX", 1, "bounce me", false);
    let reps = run(&r, "INBOX").await;
    assert!(reps[0].summary.contains("may not move mail to the Inbox"), "{}", reps[0].summary);
    assert_eq!(r.fake.uids("INBOX"), vec![1]);
    assert!(r.fake.mutations().is_empty());
    // the same rule written by the user is allowed
    r.store.lock().unwrap().delete_rule(ai).unwrap();
    let user = mk("user");
    r.runner.baseline(user).unwrap();
    r.fake.add("INBOX", 2, "bounce too", false);
    run(&r, "INBOX").await;
    assert_eq!(r.fake.uids("Postfach").len(), 1);
    // editing an enabled rule switches it off
    let id = add_rule(&r, "Receipts", receipts_rule(), true);
    assert!(r.store.lock().unwrap().get_rule(id).unwrap().unwrap().enabled);
    r.store.lock().unwrap().update_rule(id, "Receipts 2", &receipts_rule(), T0 + 1).unwrap();
    assert!(!r.store.lock().unwrap().get_rule(id).unwrap().unwrap().enabled, "an edited rule must be re-enabled by the user");
}
