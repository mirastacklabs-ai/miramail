//! Hand-written rules without any AI: the new conditions (attachment, size), the colour-label action,
//! "Apply to existing messages" (preview == what is changed, per-run undo, paused refusal) and a source scan
//! proving this path never touches the AI client. In-memory fakes only.
mod support;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use aichat::proposals::Executor;
use aichat::rules::*;
use aichat::store::Store;
use support::actions::{FakeActions, ACC};

const T0: i64 = 1_800_000_000;

struct FakeMail {
    fake: Arc<FakeActions>,
    colours: Mutex<HashMap<String, u8>>,
    /// Message-IDs whose colour cannot be set (to test the failure path).
    refuse: Mutex<HashSet<String>>,
}

impl FakeMail {
    fn facts(m: &support::actions::FMsg) -> MsgFacts {
        MsgFacts {
            from: m.from.clone(),
            to: ACC.into(),
            subject: m.subject.clone(),
            folder: m.folder.clone(),
            has_attachment: m.subject.contains("[att]"),
            size: if m.subject.contains("[big]") { 600 * 1024 } else { 10 },
            ..MsgFacts::default()
        }
    }
}

impl RuleMail for FakeMail {
    fn folders(&self, account: &str) -> Result<Vec<String>, String> {
        let mut v: Vec<String> = self.fake.st.lock().unwrap().validity.keys().filter(|(a, _)| a == account).map(|(_, f)| f.clone()).collect();
        v.sort();
        Ok(v)
    }
    fn folder_role(&self, _a: &str, folder: &str) -> Result<Option<String>, String> {
        Ok(Some(folder.to_ascii_lowercase()))
    }
    fn validity(&self, a: &str, f: &str) -> Result<Option<u32>, String> {
        Ok(self.fake.st.lock().unwrap().validity.get(&(a.into(), f.into())).copied())
    }
    fn max_uid(&self, a: &str, f: &str) -> Result<Option<u32>, String> {
        Ok(self.fake.st.lock().unwrap().msgs.iter().filter(|m| m.account == a && m.folder == f).map(|m| m.uid).max())
    }
    fn folder_exists(&self, a: &str, f: &str) -> Result<bool, String> {
        Ok(self.fake.st.lock().unwrap().validity.contains_key(&(a.into(), f.into())))
    }
    fn after(&self, a: &str, f: &str, after_uid: u32, limit: u32) -> Result<Vec<RuleMsg>, String> {
        let s = self.fake.st.lock().unwrap();
        let v = s.validity.get(&(a.into(), f.into())).copied().unwrap_or(0);
        let mut out: Vec<RuleMsg> = s
            .msgs
            .iter()
            .filter(|m| m.account == a && m.folder == f && m.uid > after_uid)
            .map(|m| RuleMsg { account: m.account.clone(), folder: m.folder.clone(), uid: m.uid, uidvalidity: v, message_id: m.message_id.clone(), facts: Self::facts(m), seen: m.seen, flagged: m.flagged, date: 100, size: 10 })
            .collect();
        out.sort_by_key(|m| m.uid);
        out.truncate(limit as usize);
        Ok(out)
    }
    fn recent(&self, a: &str, f: &str, limit: u32) -> Result<Vec<RuleMsg>, String> {
        let mut v = self.after(a, f, 0, u32::MAX)?;
        v.reverse();
        v.truncate(limit as usize);
        Ok(v)
    }
    fn colour(&self, _a: &str, mid: &str) -> Result<u8, String> {
        Ok(self.colours.lock().unwrap().get(mid).copied().unwrap_or(0))
    }
    fn set_colour(&self, _a: &str, mid: &str, c: u8) -> Result<(), String> {
        if self.refuse.lock().unwrap().contains(mid) {
            return Err("refused".into());
        }
        let mut m = self.colours.lock().unwrap();
        if c == 0 { m.remove(mid); } else { m.insert(mid.into(), c); }
        Ok(())
    }
}

struct Rig {
    store: Arc<Mutex<Store>>,
    fake: Arc<FakeActions>,
    mail: Arc<FakeMail>,
    runner: RuleRunner,
}

fn rig() -> Rig {
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let fake = Arc::new(FakeActions::new());
    fake.set_validity("Receipts", 7);
    let busy: aichat::proposals::Busy = Arc::new(Mutex::new(HashSet::new()));
    let exec = Executor::new(store.clone(), fake.clone(), Arc::new(|| T0), busy);
    let mail = Arc::new(FakeMail { fake: fake.clone(), colours: Mutex::new(HashMap::new()), refuse: Mutex::new(HashSet::new()) });
    let runner = RuleRunner::new(store.clone(), mail.clone(), exec);
    Rig { store, fake, mail, runner }
}

fn cond(field: Field, op: Op, value: &str) -> Condition {
    Condition { field, op, value: value.into() }
}

fn add(r: &Rig, spec: RuleSpec, enabled: bool) -> i64 {
    r.store.lock().unwrap().create_rule(&NewRule { account: ACC.into(), name: "manual".into(), spec, origin: "user", conv_id: None, reason: String::new(), enabled }, T0).unwrap()
}

fn spec(conds: Vec<Condition>, actions: Vec<Action>) -> RuleSpec {
    RuleSpec { v: 1, mode: MatchMode::All, conditions: conds, actions }
}

#[test]
fn attachment_and_size_conditions_match_and_validate() {
    let f = MsgFacts { has_attachment: true, size: 600 * 1024, folder: "INBOX".into(), ..MsgFacts::default() };
    let t = |c: Condition| condition_matches(&c, &f);
    assert!(t(cond(Field::HasAttachment, Op::Equals, "true")));
    assert!(!t(cond(Field::HasAttachment, Op::Equals, "false")));
    assert!(t(cond(Field::Size, Op::GreaterThan, "512")));
    assert!(!t(cond(Field::Size, Op::GreaterThan, "600")), "strictly greater");
    assert!(!t(cond(Field::Size, Op::Contains, "1")));
    let ok = |c: Condition| validate(&spec(vec![c], vec![Action::Flag]));
    assert!(ok(cond(Field::Size, Op::GreaterThan, " 0512 ")).unwrap().conditions[0].value == "512");
    for bad in ["0", "-5", "abc", "1.5", "1048577", "99999999999999999999"] {
        assert!(ok(cond(Field::Size, Op::GreaterThan, bad)).is_err(), "{bad}");
    }
    assert!(ok(cond(Field::Size, Op::Equals, "5")).is_err());
    assert!(ok(cond(Field::HasAttachment, Op::Equals, "maybe")).is_err());
    assert!(ok(cond(Field::HasAttachment, Op::Contains, "true")).is_err());
    let d = describe(&spec(vec![cond(Field::HasAttachment, Op::Equals, "true"), cond(Field::Size, Op::GreaterThan, "512")], vec![Action::Colour { colour: 3 }]));
    assert!(d.contains("has an attachment") && d.contains("larger than 512 KB") && d.contains("colour label 3"), "{d}");
}

#[test]
fn colour_action_validation_and_json_shape() {
    let c = |n: u8| validate(&spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::Colour { colour: n }]));
    assert!(c(1).is_ok() && c(7).is_ok());
    assert!(c(0).is_err() && c(8).is_err());
    let json = r#"{"v":1,"conditions":[{"field":"size","op":"greaterThan","value":"100"}],"actions":[{"type":"colour","colour":2},{"type":"flag"}]}"#;
    assert!(parse_spec(json).is_ok());
    let extra = r#"{"v":1,"conditions":[{"field":"subject","op":"contains","value":"x"}],"actions":[{"type":"flag","colour":2}]}"#;
    assert!(parse_spec(extra).is_err(), "colour key only allowed on a colour action");
    let trash = spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::Trash, Action::Colour { colour: 1 }]);
    assert!(validate(&trash).is_ok());
    assert!(describe(&trash).contains("Trash (not deleted for good)"));
}

#[tokio::test]
async fn apply_to_existing_previews_then_changes_exactly_that_and_undoes() {
    let r = rig();
    r.fake.add("INBOX", 1, "Invoice [att]", false);
    r.fake.add("INBOX", 2, "Hello", false);
    r.fake.add("INBOX", 3, "Scan [att] [big]", false);
    let s = spec(vec![cond(Field::HasAttachment, Op::Equals, "true")], vec![Action::MoveTo { folder: "Receipts".into() }, Action::MarkRead, Action::Colour { colour: 4 }]);
    // A rule that is OFF can still be applied by hand; creating it did not touch anything.
    let id = add(&r, s.clone(), false);
    assert!(r.fake.mutations().is_empty());

    let pre = r.runner.preview_existing(ACC, &s, T0).unwrap();
    assert_eq!((pre.scanned, pre.matched), (3, 2));
    assert!(r.fake.mutations().is_empty(), "preview changes nothing");

    let reps = r.runner.apply_existing(id, T0).await.unwrap();
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].matched as u32, pre.matched, "the preview count is what gets changed");
    assert_eq!(r.fake.uids("INBOX"), vec![2]);
    assert_eq!(r.fake.uids("Receipts").len(), 2);
    assert!(reps[0].summary.contains("2 moved to Receipts") && reps[0].summary.contains("2 marked read") && reps[0].summary.contains("2 labelled"), "{}", reps[0].summary);
    assert_eq!(r.mail.colours.lock().unwrap().len(), 2);

    // Applying again finds nothing new to do (the messages left the Inbox).
    assert!(r.runner.apply_existing(id, T0 + 1).await.unwrap().is_empty());

    // One undo reverts the move, the read flags and the labels.
    let un = r.runner.undo_run(reps[0].run_id).await.unwrap();
    assert_eq!(un.failed, 0, "{:?}", un.errors);
    assert_eq!(r.fake.uids("INBOX").len(), 3);
    assert!(r.mail.colours.lock().unwrap().is_empty(), "labels are put back");
    assert!(r.runner.undo_run(reps[0].run_id).await.is_err(), "second undo is refused");
}

#[tokio::test]
async fn colour_only_run_is_undoable_and_respects_hand_changes() {
    let r = rig();
    r.fake.add("INBOX", 1, "[big] one", false);
    r.fake.add("INBOX", 2, "[big] two", false);
    r.fake.add("INBOX", 3, "small", false);
    let s = spec(vec![cond(Field::Size, Op::GreaterThan, "512")], vec![Action::Colour { colour: 5 }]);
    let id = add(&r, s, false);
    r.mail.set_colour(ACC, "INBOX-2@x", 2).unwrap(); // pre-existing label on #2
    let rep = r.runner.apply_existing(id, T0).await.unwrap().remove(0);
    assert!(rep.proposal_ids.is_empty(), "colour is local: no server step");
    assert_eq!(r.fake.uids("INBOX").len(), 3, "nothing moved");
    assert_eq!(r.mail.colour(ACC, "INBOX-1@x").unwrap(), 5);
    assert_eq!(r.mail.colour(ACC, "INBOX-2@x").unwrap(), 5);
    assert_eq!(r.mail.colour(ACC, "INBOX-3@x").unwrap(), 0);
    // The user recolours #1 by hand before undoing: that one is left alone, #2 gets its old label back.
    r.mail.set_colour(ACC, "INBOX-1@x", 7).unwrap();
    let un = r.runner.undo_run(rep.run_id).await.unwrap();
    assert_eq!(un.undone, 1);
    assert_eq!(r.mail.colour(ACC, "INBOX-1@x").unwrap(), 7);
    assert_eq!(r.mail.colour(ACC, "INBOX-2@x").unwrap(), 2);
    assert!(r.runner.undo_run(rep.run_id).await.is_err());
}

#[tokio::test]
async fn trash_action_is_a_move_to_trash_never_a_delete() {
    let r = rig();
    r.fake.set_validity("Trash", 7);
    r.fake.add("INBOX", 1, "spam [big]", false);
    let id = add(&r, spec(vec![cond(Field::Size, Op::GreaterThan, "100")], vec![Action::Trash]), false);
    r.runner.apply_existing(id, T0).await.unwrap();
    assert!(r.fake.uids("INBOX").is_empty());
    assert_eq!(r.fake.uids("Trash").len(), 1);
    assert!(r.fake.mutations().iter().all(|m| !m.to_lowercase().contains("expunge") && !m.to_lowercase().contains("delete")), "{:?}", r.fake.mutations());
}

#[tokio::test]
async fn apply_is_refused_while_paused_and_for_damaged_rules() {
    let r = rig();
    r.fake.add("INBOX", 1, "x [att]", false);
    let id = add(&r, spec(vec![cond(Field::HasAttachment, Op::Equals, "true")], vec![Action::Flag]), false);
    r.store.lock().unwrap().set_rules_paused(true).unwrap();
    assert!(r.runner.apply_existing(id, T0).await.is_err());
    assert!(r.fake.mutations().is_empty());
    r.store.lock().unwrap().set_rules_paused(false).unwrap();
    assert!(r.runner.apply_existing(9999, T0).await.is_err());
    assert_eq!(r.runner.apply_existing(id, T0).await.unwrap().len(), 1);
}

#[test]
fn manual_rule_path_never_touches_the_ai_client() {
    // The rules module (spec, matcher, runner) must not reference the HTTP client or the chat engine.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rules.rs")).unwrap();
    let code = src.split("#[cfg(test)]").next().unwrap();
    for banned in ["crate::http", "crate::engine", "reqwest", "TcpStream"] {
        assert!(!code.contains(banned), "rules.rs must not use {banned}");
    }
}

#[tokio::test]
async fn apply_targets_changes_only_the_previewed_set_and_rechecks_each() {
    let r = rig();
    r.fake.add("INBOX", 1, "one [att]", false);
    r.fake.add("INBOX", 2, "two [att]", false);
    let s = spec(vec![cond(Field::HasAttachment, Op::Equals, "true")], vec![Action::Flag]);
    let id = add(&r, s.clone(), false);
    let pre = r.runner.preview_existing(ACC, &s, T0).unwrap();
    assert_eq!(pre.matched, 2);
    assert_eq!(pre.targets.len(), 2);
    let token = pre.token.clone().unwrap_or_else(|| targets_token(id, &pre.targets));
    assert_eq!(token, targets_token(id, &pre.targets));
    // Mail that arrives after the preview is NOT part of what the user confirmed.
    r.fake.add("INBOX", 3, "late [att]", false);
    // A tampered or stale token is refused and nothing changes.
    assert!(r.runner.apply_targets(id, &pre.targets, "nope", T0).await.is_err());
    assert!(r.fake.mutations().is_empty());
    let reps = r.runner.apply_targets(id, &pre.targets, &token, T0).await.unwrap();
    assert_eq!(reps.iter().map(|x| x.matched).sum::<usize>(), 2, "exactly the previewed messages");
    assert!(reps[0].summary.contains("2"), "{}", reps[0].summary);
    // A target whose UIDVALIDITY no longer matches the folder is not touched.
    let r2 = rig();
    r2.fake.add("INBOX", 1, "one [att]", false);
    let id2 = add(&r2, s.clone(), false);
    let pre2 = r2.runner.preview_existing(ACC, &s, T0).unwrap();
    let stale: Vec<Target> = pre2.targets.iter().map(|(f, u, v)| (f.clone(), *u, v + 1)).collect();
    let tok = targets_token(id2, &stale);
    assert!(r2.runner.apply_targets(id2, &stale, &tok, T0).await.unwrap().is_empty());
    assert!(r2.fake.mutations().is_empty());
}

#[tokio::test]
async fn colour_run_is_logged_before_labels_change_and_failed_labels_are_not_undone() {
    let r = rig();
    r.fake.add("INBOX", 1, "[big] one", false);
    r.fake.add("INBOX", 2, "[big] two", false);
    let s = spec(vec![cond(Field::Size, Op::GreaterThan, "512")], vec![Action::Colour { colour: 5 }]);
    let id = add(&r, s, false);
    r.mail.refuse.lock().unwrap().insert("INBOX-2@x".into());
    let rep = r.runner.apply_existing(id, T0).await.unwrap().remove(0);
    assert!(rep.summary.contains("1 labelled") && rep.summary.contains("1 could not be labelled"), "{}", rep.summary);
    assert_eq!(r.mail.colour(ACC, "INBOX-1@x").unwrap(), 5);
    assert_eq!(r.mail.colour(ACC, "INBOX-2@x").unwrap(), 0);
    let run = r.store.lock().unwrap().get_rule_run(rep.run_id).unwrap().unwrap();
    assert_eq!(run.summary, rep.summary, "the stored summary is the finished one");
    // Only the label that really changed is pending in the undo log.
    assert_eq!(r.store.lock().unwrap().pending_colour_log(rep.run_id).unwrap().len(), 1);
}
