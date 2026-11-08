//! Automation tests: in-memory ai.db, a fake mailbox and a fake outbox. Never the real Keychain, mailbox,
//! app data or SMTP server.

// The fake mailbox is shared with the AI glue tests (one copy of the support module).
use crate::ai::tests::support;
use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use support::actions::{FakeActions, ACC};

const T0: i64 = 1_800_000_000;
const DAY: i64 = 86_400;

/// Both the rule runner's and the auto-reply's view of the same fake mailbox (INBOX plus whatever the rules move).
struct World {
    fake: Arc<FakeActions>,
    /// INBOX uid -> the message as the auto-reply sees it.
    mail: Mutex<HashMap<u32, Incoming>>,
    /// Accounts `knows_account` accepts.
    known: Vec<String>,
    /// The auto-reply's cache still lists mail that a rule has already moved away (a stale local copy).
    stale: std::sync::atomic::AtomicBool,
}

impl World {
    fn new(fake: Arc<FakeActions>, known: &str) -> Arc<World> {
        Arc::new(World { fake, mail: Mutex::new(HashMap::new()), known: vec![known.to_string()], stale: Default::default() })
    }
    fn put(&self, uid: u32, m: Incoming) {
        self.fake.add("INBOX", uid, &m.subject, false);
        self.mail.lock().unwrap().insert(uid, Incoming { uid, uidvalidity: 7, ..m });
    }
}

impl InboxSource for World {
    fn knows_account(&self, a: &str) -> bool {
        self.known.iter().any(|k| k == a)
    }
    fn validity(&self, _a: &str) -> R<Option<u32>> {
        Ok(Some(7))
    }
    fn max_uid(&self, _a: &str) -> R<Option<u32>> {
        Ok(self.fake.uids("INBOX").into_iter().max().or(self.mail.lock().unwrap().keys().max().copied()))
    }
    fn after(&self, _a: &str, after_uid: u32, limit: u32) -> R<Vec<Incoming>> {
        let here: Vec<u32> = self.fake.uids("INBOX");
        let mut v: Vec<Incoming> = self.mail.lock().unwrap().values().filter(|m| m.uid > after_uid && (here.contains(&m.uid) || self.stale.load(Ordering::SeqCst))).cloned().collect();
        v.sort_by_key(|m| m.uid);
        v.truncate(limit as usize);
        Ok(v)
    }
}

impl RuleMail for World {
    fn folders(&self, account: &str) -> R<Vec<String>> {
        Ok(self.fake.st.lock().unwrap().validity.keys().filter(|(a, _)| a == account).map(|(_, f)| f.clone()).collect())
    }
    fn folder_role(&self, _a: &str, folder: &str) -> R<Option<String>> {
        Ok(Some(folder.to_ascii_lowercase()))
    }
    fn validity(&self, a: &str, folder: &str) -> R<Option<u32>> {
        Ok(self.fake.st.lock().unwrap().validity.get(&(a.into(), folder.into())).copied())
    }
    fn max_uid(&self, a: &str, folder: &str) -> R<Option<u32>> {
        Ok(self.fake.st.lock().unwrap().msgs.iter().filter(|m| m.account == a && m.folder == folder).map(|m| m.uid).max())
    }
    fn folder_exists(&self, a: &str, folder: &str) -> R<bool> {
        Ok(self.fake.st.lock().unwrap().validity.contains_key(&(a.into(), folder.into())))
    }
    fn after(&self, a: &str, folder: &str, after_uid: u32, limit: u32) -> R<Vec<RuleMsg>> {
        let s = self.fake.st.lock().unwrap();
        let mut v: Vec<RuleMsg> = s
            .msgs
            .iter()
            .filter(|m| m.account == a && m.folder == folder && m.uid > after_uid)
            .map(|m| RuleMsg {
                account: m.account.clone(),
                folder: m.folder.clone(),
                uid: m.uid,
                uidvalidity: 7,
                message_id: m.message_id.clone(),
                facts: rules::MsgFacts { from: m.from.clone(), to: a.into(), subject: m.subject.clone(), folder: m.folder.clone(), ..Default::default() },
                seen: m.seen,
                flagged: m.flagged,
                date: 100,
                size: 10,
            })
            .collect();
        v.sort_by_key(|m| m.uid);
        v.truncate(limit as usize);
        Ok(v)
    }
    fn recent(&self, a: &str, folder: &str, limit: u32) -> R<Vec<RuleMsg>> {
        let mut v = RuleMail::after(self, a, folder, 0, u32::MAX)?;
        v.reverse();
        v.truncate(limit as usize);
        Ok(v)
    }
}

#[derive(Default)]
struct FakeSender {
    sent: Mutex<Vec<(String, OutgoingMessage)>>,
    fail: Mutex<bool>,
    /// A failure that does not prove the message was not delivered.
    fail_unknown: Mutex<bool>,
    calls: AtomicUsize,
    /// Runs inside the first delivery (e.g. the user pressing the kill switch mid-pass).
    on_first: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ReplySender for FakeSender {
    fn deliver(&self, account: &str, msg: OutgoingMessage) -> BoxFut<'_, Result<(), SendFail>> {
        let account = account.to_string();
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(f) = self.on_first.lock().unwrap().take() {
                f();
            }
            if *self.fail_unknown.lock().unwrap() {
                return Err(SendFail { unknown: true, msg: "The connection was lost.".into() });
            }
            if *self.fail.lock().unwrap() {
                return Err("The mail server refused the message.".to_string().into());
            }
            self.sent.lock().unwrap().push((account, msg));
            Ok(())
        })
    }
}

struct Rig {
    auto: Automation,
    world: Arc<World>,
    fake: Arc<FakeActions>,
    sender: Arc<FakeSender>,
    now: Arc<AtomicI64>,
    account: String,
}

fn rig_for(account: &str) -> Rig {
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let fake = Arc::new(FakeActions::new());
    let world = World::new(fake.clone(), account);
    let sender = Arc::new(FakeSender::default());
    let now = Arc::new(AtomicI64::new(T0));
    let n = now.clone();
    let auto = Automation::new(
        store.clone(),
        Arc::new(Mutex::new(Default::default())),
        fake.clone(),
        world.clone(),
        world.clone(),
        sender.clone(),
        Arc::new(move || n.load(Ordering::SeqCst)),
    );
    Rig { auto, world, fake, sender, now, account: account.into() }
}

fn rig() -> Rig {
    rig_for(ACC)
}

impl Rig {
    fn at(&self, t: i64) {
        self.now.store(t, Ordering::SeqCst);
    }
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
    fn cfg(&self) -> AutoReplyConfig {
        AutoReplyConfig { enabled: true, body: "I am away until Monday.".into(), require_auth: false, ..AutoReplyConfig::default() }
    }
    fn enable(&self, cfg: AutoReplyConfig) {
        self.auto.autoreply_set(&self.account, cfg).unwrap();
    }
    /// A personal mail from `from` to this account, dated a minute ago.
    fn mail(&self, uid: u32, from: &str, subject: &str) -> Incoming {
        Incoming {
            account: self.account.clone(),
            folder_role: "inbox".into(),
            uid,
            uidvalidity: 7,
            date: self.now() - 60,
            from: from.into(),
            to: self.account.clone(),
            return_path: format!("<{}>", rules::addresses_of(from)[0]),
            subject: subject.into(),
            message_id: format!("m{uid}@{}", from.rsplit('@').next().unwrap_or("x").trim_end_matches('>')),
            headers_ok: true,
            auth_hosts: vec!["mx.example.org".into()],
            ..Incoming::default()
        }
    }
    fn put(&self, uid: u32, from: &str, subject: &str) {
        let m = self.mail(uid, from, subject);
        self.world.put(uid, m);
    }
    async fn pass(&self) -> PassReport {
        self.auto.process_new_mail(&self.account, "INBOX").await.unwrap()
    }
    fn sent(&self) -> Vec<OutgoingMessage> {
        self.sender.sent.lock().unwrap().iter().map(|(_, m)| m.clone()).collect()
    }
    fn log(&self) -> Vec<ReplyLogWire> {
        self.auto.autoreply_log(Some(&self.account), 500).unwrap()
    }
    fn reasons(&self) -> Vec<String> {
        let mut v: Vec<String> = self.log().into_iter().filter(|r| r.outcome == "skipped").map(|r| r.reason).collect();
        v.reverse();
        v
    }
}

#[tokio::test]
async fn auto_reply_is_off_by_default_reads_nothing_and_logs_nothing() {
    let r = rig();
    r.put(1, "Bob <bob@friend.test>", "Hello");
    assert!(!r.auto.autoreply_get(ACC).unwrap().enabled);
    let rep = r.pass().await;
    assert_eq!(rep.replies, ReplyStats::default());
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 0);
    assert!(r.log().is_empty());
    assert!(!r.auto.has_work(ACC));
}

#[tokio::test]
async fn old_mail_is_never_answered_new_mail_once_and_the_reply_headers_are_right() {
    let r = rig();
    r.put(1, "Old <old@friend.test>", "From before"); // already in the Inbox when the user turns it on
    r.enable(r.cfg());
    r.put(2, "Bob <bob@friend.test>", "Lunch?");
    let rep = r.pass().await;
    assert_eq!(rep.replies, ReplyStats { sent: 1, skipped: 0, failed: 0 });
    assert_eq!(r.sent().len(), 1);
    // the same message is never looked at twice
    assert_eq!(r.pass().await.replies, ReplyStats::default());
    assert_eq!(r.sent().len(), 1);

    let out = r.sent().remove(0);
    assert_eq!(out.to, vec!["bob@friend.test"]);
    assert_eq!(out.subject, "Automatic reply", "the original subject is not echoed by default");
    assert_eq!(out.text, "I am away until Monday.");
    assert!(out.cc.is_empty() && out.bcc.is_empty() && out.attachments.is_empty());
    let raw = mailcore::smtp::build_message(ACC, &out).unwrap().formatted();
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(text.contains("Auto-Submitted: auto-replied\r\n"), "{text}");
    assert!(!text.to_ascii_lowercase().contains("precedence:"), "standard header set only: {text}");
    let p = mailcore::parse_message(&raw).unwrap();
    assert_eq!(p.headers.from[0].address, ACC);
    assert_eq!(p.headers.to[0].address, "bob@friend.test");
    assert_eq!(p.headers.in_reply_to, vec!["m2@friend.test"]);
    assert_eq!(p.headers.references, vec!["m2@friend.test"]);
    assert_eq!(p.headers.subject, "Automatic reply");
    // audit log: who / when / outcome
    let log = r.log();
    assert_eq!((log[0].outcome.as_str(), log[0].sender.as_str(), log[0].ts), ("sent", "bob@friend.test", T0));
}

#[tokio::test]
async fn reply_message_threads_onto_the_original_and_validates_ids() {
    let plan = Plan {
        to: "a@b.test".into(),
        from_addr: "a@b.test".into(),
        subject: "Auto: x".into(),
        body: "b".into(),
        in_reply_to: "<p3@x>".into(),
        references: vec!["p1@x".into(), "p2@x".into(), "<p3@x>".into()],
        headers: vec![("Auto-Submitted".into(), "auto-replied".into())],
    };
    let raw = mailcore::smtp::build_message(ACC, &reply_message(&plan)).unwrap().formatted();
    let p = mailcore::parse_message(&raw).unwrap();
    assert_eq!(p.headers.in_reply_to, vec!["p3@x"]);
    assert_eq!(p.headers.references, vec!["p1@x", "p2@x", "p3@x"]);
    // a hostile Message-ID cannot inject headers: the threading headers are dropped, the mail still goes out
    let mut evil = plan.clone();
    evil.in_reply_to = "x@y>\r\nBcc: e@x.test".into();
    let raw = mailcore::smtp::build_message(ACC, &reply_message(&evil)).unwrap().formatted();
    assert!(!String::from_utf8_lossy(&raw).contains("\r\nBcc:"));
    // no parent id at all: no threading headers
    let mut none = plan;
    none.in_reply_to = String::new();
    assert!(reply_message(&none).in_reply_to.is_none());
}

#[tokio::test]
async fn one_reply_per_sender_per_seven_days() {
    let r = rig();
    r.enable(r.cfg());
    r.put(1, "Bob <bob@friend.test>", "first");
    assert_eq!(r.pass().await.replies.sent, 1);
    r.at(T0 + DAY);
    r.put(2, "Bob <bob@friend.test>", "second");
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (0, 1));
    assert_eq!(r.reasons(), vec!["sender_limit"]);
    r.at(T0 + 6 * DAY + 3600);
    r.put(3, "BOB <Bob@Friend.test>", "third, other case");
    assert_eq!(r.pass().await.replies.skipped, 1, "case-insensitive");
    r.at(T0 + 7 * DAY + 10);
    r.put(4, "Bob <bob@friend.test>", "fourth");
    assert_eq!(r.pass().await.replies.sent, 1, "allowed again after 7 days");
    assert_eq!(r.sent().len(), 2);
    // a different sender is unaffected
    r.put(5, "Carol <carol@friend.test>", "hi");
    assert_eq!(r.pass().await.replies.sent, 1);
}

#[tokio::test]
async fn daily_cap_limits_replies_across_senders() {
    let r = rig();
    r.enable(AutoReplyConfig { daily_cap: 2, ..r.cfg() });
    for (i, who) in ["a", "b", "c", "d"].iter().enumerate() {
        r.put(i as u32 + 1, &format!("{who}@friend.test"), "hi");
    }
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (2, 2));
    assert_eq!(r.reasons(), vec!["daily_cap", "daily_cap"]);
    r.at(T0 + DAY + 5);
    r.put(9, "e@friend.test", "hi");
    assert_eq!(r.pass().await.replies.sent, 1, "the cap is a rolling 24 hours");
}

#[tokio::test]
async fn only_mail_inside_the_window_is_answered() {
    let r = rig();
    r.enable(AutoReplyConfig { start: Some(T0 - 100), end: Some(T0 + 100), ..r.cfg() });
    let mut early = r.mail(1, "early@friend.test", "before the window");
    early.date = T0 - 500;
    r.world.put(1, early);
    r.put(2, "inside@friend.test", "inside");
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (1, 1));
    assert_eq!(r.reasons(), vec!["outside_window"]);
    assert_eq!(r.sent()[0].to, vec!["inside@friend.test"]);
    // after the end nothing is answered, even for fresh mail
    r.at(T0 + 200);
    r.put(3, "late@friend.test", "after the window");
    assert_eq!(r.pass().await.replies.sent, 0);
    assert_eq!(r.reasons().last().map(String::as_str), Some("outside_window"));
}

#[tokio::test]
async fn kill_switch_stops_replies_and_paused_mail_is_not_answered_later() {
    let r = rig();
    r.enable(r.cfg());
    r.auto.set_autoreply_paused(true).unwrap();
    r.put(1, "bob@friend.test", "during the pause");
    assert_eq!(r.pass().await.replies, ReplyStats::default());
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 0);
    r.auto.set_autoreply_paused(false).unwrap();
    assert_eq!(r.pass().await.replies, ReplyStats::default(), "mail from the pause stays unanswered");
    r.put(2, "carol@friend.test", "after");
    assert_eq!(r.pass().await.replies.sent, 1);
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn skipped_mail_is_logged_with_who_and_why() {
    let r = rig();
    r.enable(r.cfg());
    let mut news = r.mail(1, "News <news@shop.test>", "Sale");
    news.list_id = "<sale.shop.test>".into();
    r.world.put(1, news);
    let mut bounce = r.mail(2, "MAILER-DAEMON@mx.test", "Undelivered");
    bounce.return_path = "<>".into();
    r.world.put(2, bounce);
    r.put(3, "no-reply@bank.test", "Statement");
    let mut cc = r.mail(4, "bob@friend.test", "FYI");
    cc.to = "team@example.org".into();
    r.world.put(4, cc);
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (0, 4));
    assert_eq!(r.reasons(), vec!["list_mail", "null_return_path", "no_reply_sender", "not_addressed_to_me"]);
    let log = r.log();
    assert!(log.iter().all(|l| l.outcome == "skipped" && l.ts == T0 && !l.detail.is_empty()));
    assert!(log.iter().any(|l| l.sender == "news@shop.test"));
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_delivery_is_logged_is_not_retried_and_does_not_use_up_the_limits() {
    let r = rig();
    r.enable(AutoReplyConfig { daily_cap: 1, ..r.cfg() });
    *r.sender.fail.lock().unwrap() = true;
    r.put(1, "bob@friend.test", "hi");
    let rep = r.pass().await;
    assert_eq!(rep.replies, ReplyStats { sent: 0, skipped: 0, failed: 1 });
    assert_eq!(r.log()[0].outcome, "failed");
    assert_eq!(r.pass().await.replies, ReplyStats::default(), "no retry storm");
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 1);
    *r.sender.fail.lock().unwrap() = false;
    r.put(2, "bob@friend.test", "hi again");
    assert_eq!(r.pass().await.replies.sent, 1, "the failed attempt did not count against the sender or the cap");
}

#[tokio::test]
async fn two_auto_repliers_do_not_ping_pong() {
    const B: &str = "other@example.org";
    // Deliver what `from` sent into `to`'s Inbox the way a real server would record it (envelope sender in Return-Path).
    fn deliver(from_account: &str, to: &Rig, out: &OutgoingMessage, strip_auto_headers: bool, uid: u32) {
        let mut out = out.clone();
        if strip_auto_headers {
            out.extra_headers.clear(); // a naive responder that knows nothing about RFC 3834
        }
        let raw = mailcore::smtp::build_message(from_account, &out).unwrap().formatted();
        let p = mailcore::parse_message(&raw).unwrap();
        let x = mailcore::mime::parse_extra_headers(&raw);
        let m = Incoming {
            account: to.account.clone(),
            folder_role: "inbox".into(),
            uid,
            uidvalidity: 7,
            date: to.now() - 5,
            from: from_account.into(),
            to: to.account.clone(),
            return_path: format!("<{from_account}>"),
            subject: p.headers.subject.clone(),
            message_id: p.headers.message_id.clone().unwrap_or_default(),
            references: p.headers.references.clone(),
            auto_submitted: x.auto_submitted,
            precedence: x.precedence,
            headers_ok: true,
            ..Incoming::default()
        };
        to.world.put(uid, m);
    }

    // --- both sides follow RFC 3834 ---
    let (a, b) = (rig_for(ACC), rig_for(B));
    a.enable(a.cfg());
    b.enable(b.cfg());
    a.put(1, "Bee <other@example.org>", "Are you there?"); // a person at B writes to A
    assert_eq!(a.pass().await.replies.sent, 1);
    let reply = a.sent().remove(0);
    deliver(ACC, &b, &reply, false, 1);
    let rep = b.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (0, 1));
    assert_eq!(b.reasons(), vec!["auto_submitted"], "the second auto-replier suppresses itself");
    assert_eq!(b.sender.calls.load(Ordering::SeqCst), 0);

    // --- B is a naive responder: it answers A's auto-reply, without Auto-Submitted ---
    let (a, b) = (rig_for(ACC), rig_for(B));
    a.enable(a.cfg());
    a.put(1, "Bee <other@example.org>", "Are you there?");
    assert_eq!(a.pass().await.replies.sent, 1);
    // its reply looks like an auto reply by subject -> suppressed by the subject guard
    let mut naive = a.sent().remove(0);
    naive.subject = format!("Re: {}", naive.subject);
    let _ = &b;
    deliver(B, &a, &naive, true, 2);
    let rep = a.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (0, 1));
    assert_eq!(a.reasons(), vec!["looks_automatic"]);
    // ... and one with an innocent subject is stopped by the 7-day-per-sender limit
    let mut naive = a.sent().remove(0);
    naive.subject = "Thanks, I am away too".into();
    deliver(B, &a, &naive, true, 3);
    let rep = a.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (0, 1));
    assert_eq!(a.reasons(), vec!["looks_automatic", "sender_limit"]);
    assert_eq!(a.sender.calls.load(Ordering::SeqCst), 1, "the loop ended after the first reply");
}

#[tokio::test]
async fn concurrent_passes_send_one_reply() {
    let r = rig();
    r.enable(r.cfg());
    r.put(1, "bob@friend.test", "hi");
    let (a, b) = tokio::join!(r.pass(), r.pass());
    assert_eq!(a.replies.sent + b.replies.sent, 1);
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn preview_renders_the_reply_and_sends_nothing() {
    let r = rig();
    let mut cfg = r.cfg();
    cfg.enabled = false; // the test button works before anything is switched on
    cfg.subject = "Away: {subject}".into();
    cfg.echo_subject = true;
    let p = r.auto.autoreply_preview(ACC, &cfg, "Quarterly numbers").unwrap();
    assert_eq!(p.subject, "Away: Quarterly numbers");
    assert!(p.raw.contains("Auto-Submitted: auto-replied") && p.raw.contains("I am away until Monday.") && p.raw.contains("In-Reply-To:"), "{}", p.raw);
    assert_eq!(p.to, "sender@example.com");
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 0);
    assert!(r.log().is_empty());
    assert!(!r.auto.autoreply_get(ACC).unwrap().enabled, "a preview changes nothing");
    cfg.body = String::new();
    assert!(r.auto.autoreply_preview(ACC, &cfg, "x").is_err());
}

#[tokio::test]
async fn config_is_validated_and_enabling_never_answers_existing_mail() {
    let r = rig();
    assert!(r.auto.autoreply_set(ACC, AutoReplyConfig { enabled: true, ..Default::default() }).is_err(), "needs a text");
    assert!(r.auto.autoreply_set("not an address", r.cfg()).is_err());
    assert!(r.auto.autoreply_set("someone-else@example.org", r.cfg()).is_err(), "only a configured account");
    assert!(r.auto.autoreply_set(&ACC.to_uppercase(), r.cfg()).is_err(), "exact spelling");
    assert!(r.auto.autoreply_set(ACC, AutoReplyConfig { daily_cap: 0, ..r.cfg() }).is_err());
    r.put(1, "bob@friend.test", "waiting since yesterday");
    r.enable(r.cfg());
    assert_eq!(r.pass().await.replies, ReplyStats::default());
    assert!(r.sender.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rules_run_first_so_a_trashed_message_gets_no_reply() {
    let r = rig();
    r.enable(r.cfg());
    let spec = RuleSpec {
        v: 1,
        mode: rules::MatchMode::All,
        conditions: vec![rules::Condition { field: rules::Field::Subject, op: rules::Op::Contains, value: "lottery".into() }],
        actions: vec![rules::Action::Trash],
    };
    let w = r.auto.save_rule(RuleInput { id: None, account: ACC.into(), name: "No lottery".into(), spec }).unwrap();
    assert!(!w.enabled, "hand-written rules start off too");
    r.auto.set_rule_enabled(w.id, true).unwrap();
    r.put(1, "scam@friend.test", "You won the lottery");
    r.put(2, "bob@friend.test", "Lunch?");
    let rep = r.pass().await;
    assert_eq!(rep.rule_runs.len(), 1);
    assert_eq!(r.fake.uids("Trash").len(), 1);
    assert_eq!(rep.replies.sent, 1, "only the message that stayed in the Inbox was answered");
    assert_eq!(r.sent()[0].to, vec!["bob@friend.test"]);
    assert!(rep.affected.iter().any(|(_, f)| f == "Trash"));
    // the run is listed and can be undone from the Rules section
    let runs = r.auto.rule_runs(10).unwrap();
    assert_eq!((runs.len(), runs[0].state.as_str(), runs[0].undoable), (1, "done", true));
    let (u, _) = r.auto.undo_run(runs[0].id).await.unwrap();
    assert_eq!((u.undone, u.failed), (1, 0));
    assert_eq!(r.auto.rule_runs(10).unwrap()[0].state, "undone");
}

#[tokio::test]
async fn rule_editing_card_and_kill_switch_through_the_api() {
    let r = rig();
    let mk = |v: &str| RuleSpec {
        v: 1,
        mode: rules::MatchMode::All,
        conditions: vec![rules::Condition { field: rules::Field::Subject, op: rules::Op::Contains, value: v.into() }],
        actions: vec![rules::Action::Archive],
    };
    for i in 1..=5 {
        r.put(i, "bob@friend.test", &format!("newsletter {i}"));
    }
    let w = r.auto.save_rule(RuleInput { id: None, account: ACC.into(), name: "News".into(), spec: mk("newsletter") }).unwrap();
    let card = r.auto.rule_card(w.id).unwrap();
    assert_eq!(card.dry_run.as_ref().map(|d| d.matched), Some(5));
    assert!(card.rule.description.starts_with("When a new message arrives in the Inbox"), "{}", card.rule.description);
    assert!(r.fake.mutations().is_empty(), "the preview touches nothing");
    // edit keeps it off; a bad edit is refused and changes nothing
    let e = r.auto.save_rule(RuleInput { id: Some(w.id), account: ACC.into(), name: "News 2".into(), spec: mk("news") }).unwrap();
    assert_eq!((e.name.as_str(), e.enabled), ("News 2", false));
    assert!(r.auto.save_rule(RuleInput { id: Some(w.id), account: ACC.into(), name: "x".into(), spec: mk("") }).is_err());
    assert!(r.auto.save_rule(RuleInput { id: Some(w.id), account: "other@example.org".into(), name: "x".into(), spec: mk("a") }).is_err());
    assert_eq!(r.auto.list_rules().unwrap()[0].name, "News 2");
    // kill switch, enable, disable, delete
    r.auto.set_rules_paused(true).unwrap();
    r.auto.set_rule_enabled(w.id, true).unwrap();
    r.put(6, "bob@friend.test", "newsletter 6");
    assert!(r.pass().await.rule_runs.is_empty());
    assert!(r.fake.mutations().is_empty());
    r.auto.set_rules_paused(false).unwrap();
    r.put(7, "bob@friend.test", "newsletter 7");
    assert_eq!(r.pass().await.rule_runs.len(), 1);
    assert_eq!(r.fake.uids("Archive").len(), 1, "only the message that arrived after resuming");
    r.auto.set_rule_enabled(w.id, false).unwrap();
    r.put(8, "bob@friend.test", "newsletter 8");
    assert!(r.pass().await.rule_runs.is_empty());
    r.auto.delete_rule(w.id).unwrap();
    assert!(r.auto.list_rules().unwrap().is_empty());
    assert_eq!(r.auto.launch_folders(ACC), vec!["INBOX"]);
}

#[tokio::test]
async fn backscatter_forged_return_path_and_verp_get_no_reply() {
    let r = rig();
    r.enable(r.cfg());
    // Spam "From: bob@friend.test" with the VICTIM's address as envelope sender: answering it would bounce mail onto the victim.
    let mut forged = r.mail(1, "Bob <bob@friend.test>", "Cheap pills");
    forged.return_path = "<victim@bank.test>".into();
    r.world.put(1, forged);
    // Per-recipient (VERP) bounce address on a bulk mailer's domain
    let mut verp = r.mail(2, "Bob <bob@friend.test>", "Hello");
    verp.return_path = "<bob=friend.test@em.mailer.test>".into();
    r.world.put(2, verp);
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (0, 2));
    assert_eq!(r.reasons(), vec!["not_aligned", "not_aligned"]);
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 0, "nothing was sent to anybody");
    // the honest case (same domain) is still answered
    let mut ok = r.mail(3, "Carol <carol@friend.test>", "Hi");
    ok.return_path = "<carol@mail.friend.test>".into();
    r.world.put(3, ok);
    assert_eq!(r.pass().await.replies.sent, 1);
    assert_eq!(r.sent()[0].to, vec!["carol@mail.friend.test"]);
}

#[tokio::test]
async fn require_auth_trusts_only_our_servers_aligned_results() {
    let r = rig();
    r.enable(AutoReplyConfig { require_auth: true, ..r.cfg() });
    let with = |uid: u32, from: &str, ar: &str| {
        let mut m = r.mail(uid, from, "Hello");
        m.authentication_results = ar.to_string();
        r.world.put(uid, m);
    };
    with(1, "a@friend.test", ""); // no header at all
    with(2, "b@friend.test", "evil.test; spf=pass smtp.mailfrom=b@friend.test; dkim=pass header.d=friend.test"); // forged by the sender's side
    with(3, "c@friend.test", "mx.example.org; spf=pass smtp.mailfrom=c@elsewhere.test"); // pass, wrong domain
    with(4, "d@friend.test", "mx.example.org; dkim=pass header.d=friend.test"); // genuine
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (1, 3));
    assert_eq!(r.reasons(), vec!["auth_failed", "auth_failed", "auth_failed"]);
    assert_eq!(r.sent()[0].to, vec!["d@friend.test"]);
    assert!(r.auto.autoreply_get(ACC).unwrap().require_auth, "ON by default for new configurations");
}

#[tokio::test]
async fn a_row_without_stored_guard_headers_is_never_answered() {
    let r = rig();
    r.enable(r.cfg());
    // Torn write / row from before the migration: the Auto-Submitted and List-Id headers read as empty.
    let mut torn = r.mail(1, "Robot <robot@friend.test>", "Weekly digest");
    torn.headers_ok = false;
    r.world.put(1, torn);
    r.put(2, "bob@friend.test", "Lunch?");
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (1, 1));
    assert_eq!(r.reasons(), vec!["headers_missing"]);
    assert_eq!(r.sent()[0].to, vec!["bob@friend.test"]);
}

#[tokio::test]
async fn cache_rows_written_without_headers_read_as_headers_missing() {
    use mailcore::{AccountConfig, Flags, Folder, FolderRole, MessageSummary, ServerConfig, Tls};
    const ME: &str = "me@example.org";
    let storage = Arc::new(Storage::open_in_memory().unwrap());
    storage
        .upsert_account(&AccountConfig {
            email: ME.into(),
            username: "me".into(),
            imap: ServerConfig { host: "imap.example.org".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "smtp.example.org".into(), port: 587, tls: Tls::StartTls },
        })
        .unwrap();
    storage.replace_folders(ME, &[Folder { name: "INBOX".into(), delimiter: None, role: FolderRole::Inbox, selectable: true }]).unwrap();
    let msg = |uid: u32| MessageSummary {
        account: ME.into(), folder: "INBOX".into(), uid, uidvalidity: 7, message_id: Some(format!("m{uid}@x")), subject: "s".into(), from: "a@friend.test".into(), to: ME.into(),
        date: T0, flags: Flags::default(), snippet: String::new(), has_body: false, size: 10,
    };
    storage.upsert_message(&msg(1)).unwrap(); // summary only
    let h = mailcore::mime::parse_extra_headers(b"Return-Path: <a@friend.test>\r\nAuto-Submitted: auto-generated\r\n\r\n");
    storage.upsert_message_with_headers(&msg(2), &h).unwrap();
    let cache = CacheMail::new(storage);
    let rows = InboxSource::after(&cache, ME, 0, 10).unwrap();
    assert_eq!(rows.iter().map(|m| (m.uid, m.headers_ok)).collect::<Vec<_>>(), vec![(1, false), (2, true)]);
    assert_eq!(rows[1].auto_submitted, "auto-generated");
    assert!(rows[0].auth_hosts.contains(&"imap.example.org".to_string()) && rows[0].auth_hosts.contains(&"example.org".to_string()));
    assert!(InboxSource::knows_account(&cache, ME) && !InboxSource::knows_account(&cache, "ME@example.org"));
    // and decide() refuses the first, accepts the second only for what its headers say
    let cfg = AutoReplyConfig { enabled: true, body: "b".into(), require_auth: false, ..AutoReplyConfig::default() };
    let env = Env { now: T0 + 5, paused: false };
    assert_eq!(autoreply::decide(&cfg, &rows[0], History::default(), env).unwrap_err(), Skip::HeadersMissing);
    assert_eq!(autoreply::decide(&cfg, &rows[1], History::default(), env).unwrap_err(), Skip::AutoSubmitted);
}

#[tokio::test]
async fn pausing_in_the_middle_of_a_pass_stops_the_remaining_replies() {
    let r = rig();
    r.enable(r.cfg());
    for (i, who) in ["a", "b", "c"].iter().enumerate() {
        r.put(i as u32 + 1, &format!("{who}@friend.test"), "hi");
    }
    // The user hits the kill switch while the first reply is being delivered.
    let store = r.auto.store.clone();
    *r.sender.on_first.lock().unwrap() = Some(Box::new(move || {
        store.lock().unwrap().set_autoreply_paused(true).unwrap();
    }));
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (1, 2));
    assert_eq!(r.reasons(), vec!["paused", "paused"]);
    assert_eq!(r.sender.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_sender_limit_covers_from_and_return_path_and_ambiguous_failures_count() {
    let r = rig();
    r.enable(r.cfg());
    let mut m = r.mail(1, "Bob <bob@friend.test>", "first");
    m.return_path = "<mailer1@friend.test>".into();
    r.world.put(1, m);
    assert_eq!(r.pass().await.replies.sent, 1);
    // another person, but the same envelope sender
    let mut m = r.mail(2, "Eve <eve@friend.test>", "second");
    m.return_path = "<mailer1@friend.test>".into();
    r.world.put(2, m);
    // the same person from a different envelope sender
    let mut m = r.mail(3, "Bob <bob@friend.test>", "third");
    m.return_path = "<mailer2@friend.test>".into();
    r.world.put(3, m);
    let rep = r.pass().await;
    assert_eq!((rep.replies.sent, rep.replies.skipped), (0, 2));
    assert_eq!(r.reasons(), vec!["sender_limit", "sender_limit"]);
    // A connection that broke after the message may have been accepted keeps counting; a clean refusal does not.
    *r.sender.fail_unknown.lock().unwrap() = true;
    r.put(4, "carol@friend.test", "hi");
    assert_eq!(r.pass().await.replies.failed, 1);
    assert_eq!(r.log()[0].outcome, "unknown");
    *r.sender.fail_unknown.lock().unwrap() = false;
    r.put(5, "carol@friend.test", "hi again");
    assert_eq!(r.pass().await.replies.sent, 0, "a possibly-sent reply still counts against the 7-day limit");
    assert_eq!(r.reasons().last().map(String::as_str), Some("sender_limit"));
}

#[tokio::test]
async fn mail_a_rule_just_moved_is_not_answered_even_if_the_cache_still_lists_it() {
    let r = rig();
    r.enable(r.cfg());
    let spec = RuleSpec {
        v: 1,
        mode: rules::MatchMode::All,
        conditions: vec![rules::Condition { field: rules::Field::Subject, op: rules::Op::Contains, value: "receipt".into() }],
        actions: vec![rules::Action::Archive],
    };
    let w = r.auto.save_rule(RuleInput { id: None, account: ACC.into(), name: "Receipts".into(), spec }).unwrap();
    r.auto.set_rule_enabled(w.id, true).unwrap();
    r.world.stale.store(true, Ordering::SeqCst);
    r.put(1, "shop@friend.test", "your receipt");
    r.put(2, "bob@friend.test", "Lunch?");
    let rep = r.pass().await;
    assert_eq!(rep.rule_runs.len(), 1);
    assert_eq!(rep.replies.sent, 1);
    assert_eq!(r.sent()[0].to, vec!["bob@friend.test"], "only the message that stayed was answered");
}

#[tokio::test]
async fn editing_an_enabled_rule_turns_it_off() {
    let r = rig();
    let mk = |v: &str| RuleSpec {
        v: 1,
        mode: rules::MatchMode::All,
        conditions: vec![rules::Condition { field: rules::Field::Subject, op: rules::Op::Contains, value: v.into() }],
        actions: vec![rules::Action::Archive],
    };
    let w = r.auto.save_rule(RuleInput { id: None, account: ACC.into(), name: "News".into(), spec: mk("news") }).unwrap();
    assert!(r.auto.set_rule_enabled(w.id, true).unwrap().enabled);
    let e = r.auto.save_rule(RuleInput { id: Some(w.id), account: ACC.into(), name: "News".into(), spec: mk("newsletter") }).unwrap();
    assert!(!e.enabled, "an edited rule must be previewed and enabled again");
}

/// The automation code has no path to the AI endpoint: its source names neither the HTTP client, the chat
/// engine, the providers nor the chat glue (`crate::ai`), and the chat glue never names this module.
#[test]
fn automation_code_has_no_dependency_on_the_ai_client() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let banned = ["aichat::http", "aichat::engine", "aichat::provider", "aichat::sse", "aichat::budget", "aichat::context", "reqwest", "crate::ai", "ai::AiState", "ProfileRuntime", "TurnCtx", "run_turn"];
    let own = std::fs::read_to_string(root.join("automation.rs")).unwrap();
    for (i, line) in own.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for b in banned {
            assert!(!line.contains(b), "automation.rs:{}: `{b}` in `{}`", i + 1, line.trim());
        }
    }
    // the chat glue cannot reach it either
    for e in std::fs::read_dir(root.join("ai")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("rs") || p.file_name().and_then(|x| x.to_str()) == Some("tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for b in ["crate::automation", "Automation", "ReplySender", "AutoReplyConfig", "autoreply::", "RuleRunner"] {
                assert!(!line.contains(b), "{}:{}: `{b}`", p.display(), i + 1);
            }
        }
    }
    // the manifest of the pure crate still has no mail or SMTP dependency
    let manifest = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/aichat/Cargo.toml")).unwrap().to_lowercase();
    for bad in ["lettre", "mailcore", "async-imap"] {
        assert!(!manifest.contains(bad), "aichat must not depend on {bad}");
    }
}

#[tokio::test]
async fn manual_rule_is_created_previewed_applied_and_undone_with_no_ai_profile() {
    // No AI profile exists anywhere in this rig: rules need only the local store and the local mailbox.
    let r = rig();
    r.put(1, "Shop <orders@shop.test>", "Your receipt");
    r.put(2, "Bob <bob@friend.test>", "Lunch?");
    r.fake.set_validity("Receipts", 7);
    let spec = rules::RuleSpec {
        v: 1,
        mode: rules::MatchMode::All,
        conditions: vec![rules::Condition { field: rules::Field::Subject, op: rules::Op::Contains, value: "receipt".into() }],
        actions: vec![rules::Action::MoveTo { folder: "Receipts".into() }, rules::Action::MarkRead],
    };
    let w = r.auto.save_rule(RuleInput { id: None, account: ACC.into(), name: "Receipts".into(), spec }).unwrap();
    assert!(!w.enabled && w.origin == "user", "hand-written rules start off");
    assert!(r.fake.mutations().is_empty());
    let pre = r.auto.preview_existing(w.id).unwrap();
    assert_eq!((pre.scanned, pre.matched), (2, 1));
    assert!(r.fake.mutations().is_empty(), "preview changes nothing");

    let (runs, affected) = r.auto.apply_existing(w.id, None).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].matched, runs[0].undoable), (1, true));
    assert!(affected.iter().any(|(_, f)| f == "Receipts"));
    assert_eq!(r.fake.uids("Receipts").len(), 1);
    assert_eq!(r.fake.uids("INBOX"), vec![2]);

    let (u, _) = r.auto.undo_run(runs[0].id).await.unwrap();
    assert_eq!(u.failed, 0);
    assert_eq!(r.fake.uids("INBOX").len(), 2);
    assert_eq!(r.auto.rule_runs(10).unwrap()[0].state, "undone");

    // Pause all blocks applying.
    r.auto.set_rules_paused(true).unwrap();
    assert!(r.auto.apply_existing(w.id, None).await.is_err());
}
