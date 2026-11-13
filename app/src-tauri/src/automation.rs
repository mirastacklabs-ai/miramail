//! Local automation (AI chat P4): rules and the opt-in template auto-reply. **No model and no AI endpoint are
//! involved here**: this file never names the AI HTTP client, the chat engine or the providers (a test scans
//! it). Automation runs ONLY while MiraMail is open; the UI says so and suggests the provider's own vacation
//! responder for the rest of the time. On iPhone "open" means the user is in front of it (presence.rs): nothing
//! here runs during a background refresh; that mail is handled when the user is back.
//!
//! * Triggered after a folder was synced (INBOX watcher, live folders, background sync, manual sync) and once
//!   shortly after launch. One pass per account at a time.
//! * Rules run first (they may move or trash a message), then the auto-reply looks at what is still in the
//!   Inbox. Both walk NEW mail only, with a cursor per UIDVALIDITY, so a message is handled once.
//! * Auto-reply is delivered through the account's OWN SMTP server with the account's own credentials, the
//!   same way Compose sends, and only after the audit log slot was reserved atomically.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aichat::autoreply::{self, AutoReplyConfig, Env, History, InboxSource, Incoming, LogRow as ReplyLog, Plan, ReplyOutcome, Skip};
use aichat::proposals::{self, Executor, MailActionHost};
use aichat::rules::{self, Rule, RuleMail, RuleMsg, RuleRunner, RuleSpec};
use aichat::store::Store;
use aichat::tools::BoxFut;
use mailcore::{OutgoingMessage, ReplyTo, Storage};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

type R<T> = Result<T, String>;
type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

const DB_ERR: &str = "MiraMail couldn't read or write its automation database.";
/// Newest-first window of the Inbox a reply pass will read per step.
const REPLY_BATCH: u32 = 100;
const REPLY_MAX_BATCHES: usize = 10;
pub const RUN_KEEP_SECS: i64 = 90 * 86_400;

pub fn system_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn db<E>(_: E) -> String {
    DB_ERR.to_string()
}

fn lock(s: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Reading the local cache
// ---------------------------------------------------------------------------------------------

/// Cache reader for rules and auto-reply. Never touches the network.
pub struct CacheMail {
    storage: Arc<Storage>,
}

impl CacheMail {
    pub fn new(storage: Arc<Storage>) -> Self {
        CacheMail { storage }
    }
    fn facts(m: &mailcore::MessageSummary, x: &mailcore::mime::ExtraHeaders) -> rules::MsgFacts {
        rules::MsgFacts {
            from: m.from.clone(),
            to: m.to.clone(),
            cc: x.cc.clone(),
            subject: m.subject.clone(),
            list_id: x.list_id.clone(),
            has_list_unsubscribe: !x.list_unsubscribe.trim().is_empty(),
            has_attachment: x.attachment_hint,
            size: m.size,
            folder: m.folder.clone(),
        }
    }
    /// Names the account's own mail server may sign `Authentication-Results` with: its IMAP host and the
    /// domain of the account address. Empty when the account is unknown (then no result is trusted).
    fn auth_hosts(&self, account: &str) -> Vec<String> {
        let Ok(Some(cfg)) = self.storage.get_account(account) else { return vec![] };
        let mut v = vec![cfg.imap.host.to_ascii_lowercase()];
        if let Some((_, d)) = cfg.email.rsplit_once('@') {
            v.push(d.to_ascii_lowercase());
        }
        v.retain(|h| !h.is_empty());
        v
    }
    fn rule_msgs(&self, account: &str, folder: &str, after: u32, limit: u32, newest_first: bool) -> R<Vec<RuleMsg>> {
        let rows = self.storage.list_with_headers(account, folder, after, limit, newest_first).map_err(db)?;
        Ok(rows
            .iter()
            .map(|(m, x)| RuleMsg {
                account: m.account.clone(),
                folder: m.folder.clone(),
                uid: m.uid,
                uidvalidity: m.uidvalidity,
                message_id: m.message_id.clone(),
                facts: Self::facts(m, x),
                seen: m.flags.seen,
                flagged: m.flags.flagged,
                date: m.date,
                size: m.size,
            })
            .collect())
    }
}

impl RuleMail for CacheMail {
    fn folders(&self, account: &str) -> R<Vec<String>> {
        Ok(self.storage.list_folders(account).map_err(db)?.into_iter().filter(|f| f.selectable).map(|f| f.name).collect())
    }
    fn folder_role(&self, account: &str, folder: &str) -> R<Option<String>> {
        Ok(self.storage.list_folders(account).map_err(db)?.into_iter().find(|f| f.name == folder).map(|f| f.role.as_str().to_string()))
    }
    fn validity(&self, account: &str, folder: &str) -> R<Option<u32>> {
        Ok(self.storage.folder_sync_state(account, folder).map_err(db)?.0)
    }
    fn max_uid(&self, account: &str, folder: &str) -> R<Option<u32>> {
        self.storage.max_uid(account, folder).map_err(db)
    }
    fn folder_exists(&self, account: &str, folder: &str) -> R<bool> {
        Ok(self.storage.list_folders(account).map_err(db)?.iter().any(|f| f.name == folder))
    }
    fn after(&self, account: &str, folder: &str, after_uid: u32, limit: u32) -> R<Vec<RuleMsg>> {
        self.rule_msgs(account, folder, after_uid, limit, false)
    }
    fn recent(&self, account: &str, folder: &str, limit: u32) -> R<Vec<RuleMsg>> {
        self.rule_msgs(account, folder, 0, limit, true)
    }
    fn colour(&self, account: &str, message_id: &str) -> R<u8> {
        self.storage.get_colour(account, message_id).map_err(db)
    }
    fn set_colour(&self, account: &str, message_id: &str, colour: u8) -> R<()> {
        self.storage.set_colour(account, message_id, colour).map_err(db)
    }
}

impl InboxSource for CacheMail {
    fn knows_account(&self, account: &str) -> bool {
        matches!(self.storage.get_account(account), Ok(Some(_)))
    }
    fn validity(&self, account: &str) -> R<Option<u32>> {
        RuleMail::validity(self, account, "INBOX")
    }
    fn max_uid(&self, account: &str) -> R<Option<u32>> {
        RuleMail::max_uid(self, account, "INBOX")
    }
    fn after(&self, account: &str, after_uid: u32, limit: u32) -> R<Vec<Incoming>> {
        let rows = self.storage.list_with_headers(account, "INBOX", after_uid, limit, false).map_err(db)?;
        let auth_hosts = self.auth_hosts(account);
        Ok(rows
            .into_iter()
            .map(|(m, x)| Incoming {
                account: account.to_string(),
                folder_role: "inbox".into(),
                uid: m.uid,
                uidvalidity: m.uidvalidity,
                date: m.date,
                from: m.from,
                to: m.to,
                cc: x.cc,
                return_path: x.return_path,
                subject: m.subject,
                message_id: m.message_id.unwrap_or_default(),
                references: x.references.split_whitespace().map(str::to_string).collect(),
                auto_submitted: x.auto_submitted,
                precedence: x.precedence,
                list_id: x.list_id,
                list_unsubscribe: x.list_unsubscribe,
                authentication_results: x.authentication_results,
                headers_ok: x.headers_ok,
                auth_hosts: auth_hosts.clone(),
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------------------------
// Delivery of an auto-reply
// ---------------------------------------------------------------------------------------------

/// Delivers a finished auto-reply through the account's own mail server. Faked in tests.
pub trait ReplySender: Send + Sync {
    fn deliver(&self, account: &str, msg: OutgoingMessage) -> BoxFut<'_, Result<(), SendFail>>;
}

/// Why a delivery did not complete. `unknown` = the connection broke at a point where the server may still have
/// accepted the message: the reply keeps counting against the limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendFail {
    pub unknown: bool,
    pub msg: String,
}

impl From<String> for SendFail {
    fn from(msg: String) -> Self {
        SendFail { unknown: false, msg }
    }
}

/// The reply message for a decided [`Plan`]: threaded onto the original, `Auto-Submitted: auto-replied`.
pub fn reply_message(plan: &Plan) -> OutgoingMessage {
    let parent = plan.in_reply_to.trim();
    let refs: Vec<String> = plan.references.iter().filter(|r| r.trim() != parent).cloned().collect();
    OutgoingMessage {
        to: vec![plan.to.clone()],
        cc: vec![],
        bcc: vec![],
        subject: plan.subject.clone(),
        text: plan.body.clone(),
        html: None,
        attachments: vec![],
        in_reply_to: (!parent.is_empty()).then(|| ReplyTo { message_id: parent.to_string(), references: refs }),
        calendar: None,
        extra_headers: plan.headers.clone(),
    }
}

// ---------------------------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyStats {
    pub sent: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassReport {
    pub rule_runs: Vec<rules::RunReport>,
    pub replies: ReplyStats,
    /// `(account, folder)` pairs whose contents changed.
    pub affected: Vec<(String, String)>,
}

pub struct Automation {
    store: Arc<Mutex<Store>>,
    busy: proposals::Busy,
    actions: Arc<dyn MailActionHost>,
    mail: Arc<dyn RuleMail>,
    inbox: Arc<dyn InboxSource>,
    sender: Arc<dyn ReplySender>,
    now: NowFn,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Latest "Apply to existing" preview per rule: its token and the exact messages it matched.
    previews: Mutex<HashMap<i64, (String, Vec<rules::Target>)>>,
}

/// The Rules screen's "turn on" step for the AI executor: start the rule's cursors at "now" (new mail only).
pub struct AppRuleHook {
    pub store: Arc<Mutex<Store>>,
    pub busy: proposals::Busy,
    pub actions: Arc<dyn MailActionHost>,
    pub mail: Arc<dyn RuleMail>,
    pub now: NowFn,
}

impl proposals::RuleHook for AppRuleHook {
    fn baseline(&self, rule_id: i64) -> Result<(), String> {
        let ex = Executor::new(self.store.clone(), self.actions.clone(), self.now.clone(), self.busy.clone());
        RuleRunner::new(self.store.clone(), self.mail.clone(), ex).baseline(rule_id)
    }
}

impl Automation {
    pub fn new(
        store: Arc<Mutex<Store>>,
        busy: proposals::Busy,
        actions: Arc<dyn MailActionHost>,
        mail: Arc<dyn RuleMail>,
        inbox: Arc<dyn InboxSource>,
        sender: Arc<dyn ReplySender>,
        now: NowFn,
    ) -> Self {
        Automation { store, busy, actions, mail, inbox, sender, now, locks: Mutex::new(HashMap::new()), previews: Mutex::new(HashMap::new()) }
    }

    fn account_lock(&self, account: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap_or_else(|e| e.into_inner()).entry(account.to_string()).or_default().clone()
    }

    fn executor(&self) -> Executor {
        Executor::new(self.store.clone(), self.actions.clone(), self.now.clone(), self.busy.clone())
    }

    pub fn runner(&self) -> RuleRunner {
        RuleRunner::new(self.store.clone(), self.mail.clone(), self.executor())
    }

    /// True when the account has any enabled rule or an enabled auto-reply (so a launch pass is worth a connection).
    pub fn has_work(&self, account: &str) -> bool {
        let mut st = lock(&self.store);
        let rules = st.list_rules(Some(account)).map(|r| r.iter().any(|r| r.enabled)).unwrap_or(false);
        rules || st.autoreply_config(account).map(|c| c.enabled).unwrap_or(false)
    }

    /// Folders to look at on launch: the Inbox plus every folder an enabled rule names exactly.
    pub fn launch_folders(&self, account: &str) -> Vec<String> {
        let mut out = vec!["INBOX".to_string()];
        let rules = lock(&self.store).list_rules(Some(account)).unwrap_or_default();
        for r in rules.iter().filter(|r| r.enabled) {
            for c in r.spec.iter().flat_map(|s| s.conditions.iter()) {
                if c.field == rules::Field::Folder && c.op == rules::Op::Equals && !out.contains(&c.value) {
                    out.push(c.value.clone());
                }
            }
        }
        out
    }

    /// The whole automation pass for one synced folder. Serialised per account.
    pub async fn process_new_mail(&self, account: &str, folder: &str) -> R<PassReport> {
        let lk = self.account_lock(account);
        let _g = lk.lock().await;
        let now = (self.now)();
        let mut out = PassReport::default();
        let runs = self.runner().run_folder(account, folder, now).await?;
        for r in &runs {
            out.affected.extend(r.affected.iter().cloned());
        }
        // Mail a rule just moved away must not be answered, even if the local cache still lists it.
        let moved: std::collections::HashSet<u32> = if folder.eq_ignore_ascii_case("INBOX") { runs.iter().flat_map(|r| r.moved_uids.iter().copied()).collect() } else { Default::default() };
        out.rule_runs = runs;
        if folder.eq_ignore_ascii_case("INBOX") {
            out.replies = self.autoreply_pass(account, now, &moved).await?;
        }
        out.affected.sort();
        out.affected.dedup();
        Ok(out)
    }

    /// Answer new Inbox mail with the template, if (and only if) the user turned that on for this account.
    pub async fn autoreply_pass(&self, account: &str, now: i64, moved: &std::collections::HashSet<u32>) -> R<ReplyStats> {
        let mut stats = ReplyStats::default();
        let (cfg, paused) = {
            let mut st = lock(&self.store);
            (st.autoreply_config(account).map_err(db)?, st.autoreply_paused().map_err(db)?)
        };
        if !cfg.enabled {
            return Ok(stats); // OFF: nothing is read, nothing is logged
        }
        let Some(v) = self.inbox.validity(account)? else { return Ok(stats) };
        let max = self.inbox.max_uid(account)?.unwrap_or(0);
        let cur = lock(&self.store).autoreply_cursor(account).map_err(db)?;
        let mut last = match cur {
            Some((cv, lu)) if cv == v => lu,
            // First pass, or the folder was reset on the server: start from "now". Old mail is never answered.
            _ => {
                lock(&self.store).set_autoreply_cursor(account, v, max).map_err(db)?;
                return Ok(stats);
            }
        };
        if paused {
            if max > last {
                lock(&self.store).set_autoreply_cursor(account, v, max).map_err(db)?;
            }
            return Ok(stats);
        }
        for _ in 0..REPLY_MAX_BATCHES {
            let batch = self.inbox.after(account, last, REPLY_BATCH)?;
            if batch.is_empty() {
                break;
            }
            let full = batch.len() as u32 >= REPLY_BATCH;
            let top = batch.iter().map(|m| m.uid).max().unwrap_or(last);
            // Advance BEFORE answering: a crash never makes the same message get a second reply.
            lock(&self.store).set_autoreply_cursor(account, v, top).map_err(db)?;
            last = top;
            for m in batch.iter().filter(|m| m.uidvalidity == v && !moved.contains(&m.uid)) {
                // One message's database trouble must not abort the rest of the batch (the cursor already moved).
                if self.one_reply(m, now, &mut stats).await.is_err() {
                    stats.failed += 1;
                }
            }
            if !full {
                break;
            }
        }
        Ok(stats)
    }

    async fn one_reply(&self, m: &Incoming, now: i64, stats: &mut ReplyStats) -> R<()> {
        let account = m.account.as_str();
        let who = rules::addresses_of(&m.from).into_iter().next().unwrap_or_else(|| m.from.chars().take(80).collect());
        let skip = |stats: &mut ReplyStats, why: Skip| -> R<()> {
            stats.skipped += 1;
            lock(&self.store).log_skip(account, &who, &m.subject, why, now).map_err(db)
        };
        // Re-read the switches for every message: a pause or an edit during the pass takes effect at once.
        let (cfg, paused) = {
            let mut st = lock(&self.store);
            (st.autoreply_config(account).map_err(db)?, st.autoreply_paused().map_err(db)?)
        };
        // Limits are decided atomically by `reserve_reply`; `decide` runs with an empty history first.
        let plan = match autoreply::decide(&cfg, m, History::default(), Env { now, paused }) {
            Ok(p) => p,
            Err(why) => return skip(stats, why),
        };
        // (bind first: a guard held in the `match` scrutinee would still be alive inside `skip`)
        let reserved = lock(&self.store).reserve_reply(account, &plan.from_addr, &plan.to, &m.subject, cfg.daily_cap, now).map_err(db)?;
        let id = match reserved {
            Ok(id) => id,
            Err(why) => return skip(stats, why),
        };
        match self.sender.deliver(account, reply_message(&plan)).await {
            Ok(()) => {
                lock(&self.store).finish_reply(id, ReplyOutcome::Sent, "").map_err(db)?;
                stats.sent += 1;
            }
            Err(e) => {
                let outcome = if e.unknown { ReplyOutcome::Unknown } else { ReplyOutcome::Failed };
                lock(&self.store).finish_reply(id, outcome, &e.msg).map_err(db)?;
                stats.failed += 1;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Real delivery + cache reader wiring (Tauri)
// ---------------------------------------------------------------------------------------------

pub struct AppSender {
    pub app: AppHandle,
}

impl ReplySender for AppSender {
    fn deliver(&self, account: &str, msg: OutgoingMessage) -> BoxFut<'_, Result<(), SendFail>> {
        let account = account.to_string();
        Box::pin(async move {
            let st = self.app.state::<crate::state::AppState>();
            let cfg = st.account(&account)?;
            let password = st.password(&account)?;
            let raw = mailcore::smtp::send(&cfg, &password, &msg).await.map_err(|e| SendFail { unknown: e.outcome_unknown(), msg: e.to_string() })?;
            // The reply is out. A copy in Sent is best effort, like Compose.
            // Same duplicate-safe Sent copy as Compose; in the background so the reply loop is not held up by the wait.
            let app = self.app.clone();
            tauri::async_runtime::spawn(async move { crate::outbox::save_to_sent(&app, &account, &raw).await });
            Ok(())
        })
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutomationEvent {
    kind: &'static str,
}

fn emit_changed(app: &AppHandle, affected: &[(String, String)]) {
    let mut by_account: std::collections::BTreeMap<&str, Vec<String>> = std::collections::BTreeMap::new();
    for (a, f) in affected {
        by_account.entry(a.as_str()).or_default().push(f.clone());
    }
    for (account, folders) in by_account {
        let _ = app.emit("mail://synced", crate::wire::SyncedEvent { account: account.to_string(), folders });
    }
}

/// Called after a folder of `account` was synced and got new mail. Runs in the background; a failure only
/// means "nothing ran this time".
///
/// iPhone: does nothing while the user is away (presence.rs). The rule and auto-reply cursors then stay where they
/// are, so the next pass once the user is back (`start_catch_up_pass`, or the launch pass) handles that mail, once.
pub fn notify(app: &AppHandle, account: &str, folder: &str) {
    if !crate::presence::user_present() {
        return;
    }
    let (app, account, folder) = (app.clone(), account.to_string(), folder.to_string());
    tauri::async_runtime::spawn(async move {
        let Some(auto) = app.try_state::<Automation>() else { return };
        if !crate::presence::user_present() {
            return;
        }
        if let Ok(rep) = auto.process_new_mail(&account, &folder).await {
            emit_changed(&app, &rep.affected);
            if !rep.rule_runs.is_empty() {
                let _ = app.emit("ai://automation", AutomationEvent { kind: "rules" });
            }
            if rep.replies != ReplyStats::default() {
                let _ = app.emit("ai://automation", AutomationEvent { kind: "autoreply" });
            }
        }
    });
}

/// Shortly after launch: for every account with something enabled, sync the Inbox once (mail that arrived while
/// MiraMail was closed) and run the pass. Talks only to the user's own mail server.
///
/// iPhone: starts only once the user is present (lib.rs) and stops if they leave; whatever it skipped is picked up by
/// the catch-up pass when they are back (the cursors did not move).
pub fn start_launch_pass(app: &AppHandle, accounts: Vec<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        let Some(auto) = app.try_state::<Automation>() else { return };
        let _ = lock(&auto.store).prune_rule_runs(system_now() - RUN_KEEP_SECS);
        for account in accounts {
            if !crate::presence::user_present() {
                return;
            }
            if !auto.has_work(&account) {
                continue;
            }
            let st = app.state::<crate::state::AppState>();
            if let Ok(mut conn) = st.connect(&account).await {
                let _ = conn.client.sync_folder(&st.storage, &account, "INBOX", st.initial_sync_limit()).await;
                conn.close().await;
            }
            for folder in auto.launch_folders(&account) {
                notify(&app, &account, &folder);
            }
        }
    });
}

/// iPhone, the user is back after being away: run the pass over the Inbox and the folders rules name, for mail that
/// arrived meanwhile (stored by the background check, or synced while the watchers were stopping). No extra sync: the
/// cache is read, and the watchers that just restarted / the page's own sync bring in anything newer (and call
/// [`notify`] again). The rule and auto-reply cursors make a repeat a no-op, so nothing is handled twice.
pub fn start_catch_up_pass(app: &AppHandle, accounts: Vec<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(auto) = app.try_state::<Automation>() else { return };
        for account in accounts {
            if !auto.has_work(&account) {
                continue;
            }
            for folder in auto.launch_folders(&account) {
                notify(&app, &account, &folder);
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Wire types + commands
// ---------------------------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleWire {
    pub id: i64,
    pub account: String,
    pub name: String,
    pub enabled: bool,
    /// `user` | `ai`
    pub origin: String,
    pub reason: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_run_at: Option<i64>,
    /// false = the stored rule no longer validates; it never runs.
    pub valid: bool,
    pub spec: Option<RuleSpec>,
    /// Plain-English description written by the app.
    pub description: String,
}

fn rule_wire(r: &Rule) -> RuleWire {
    RuleWire {
        id: r.id,
        account: r.account.clone(),
        name: r.name.clone(),
        enabled: r.enabled,
        origin: r.origin.clone(),
        reason: aichat::envelope::clean_line(&r.reason, 500),
        created_at: r.created_at,
        updated_at: r.updated_at,
        last_run_at: r.last_run_at,
        valid: r.spec.is_some(),
        spec: r.spec.clone(),
        description: r.spec.as_ref().map(rules::describe).unwrap_or_else(|| "This rule is damaged and will not run.".into()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleInput {
    pub id: Option<i64>,
    pub account: String,
    pub name: String,
    pub spec: RuleSpec,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRowWire {
    pub folder: String,
    pub from: String,
    pub subject: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunWire {
    pub scanned: u32,
    pub matched: u32,
    pub sample: Vec<DryRowWire>,
    /// "Apply to existing" previews: pass back to `ai_rule_apply_existing` so exactly these messages are changed.
    pub token: Option<String>,
}

fn dry_wire(d: rules::DryRun) -> DryRunWire {
    DryRunWire { scanned: d.scanned, matched: d.matched, token: d.token, sample: d.sample.into_iter().map(|r| DryRowWire { folder: r.folder, from: r.from, subject: r.subject }).collect() }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCardWire {
    pub rule: RuleWire,
    pub dry_run: Option<DryRunWire>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunWire {
    pub id: i64,
    pub rule_id: i64,
    pub rule_name: String,
    pub account: String,
    pub folder: String,
    pub ts: i64,
    pub matched: u32,
    pub summary: String,
    /// `done` | `partial` | `undone` | `failed`
    pub state: String,
    pub undoable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoRunWire {
    pub undone: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationState {
    pub rules_paused: bool,
    pub autoreply_paused: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyLogWire {
    pub id: i64,
    pub account: String,
    pub ts: i64,
    pub sender: String,
    pub subject: String,
    /// `sent` | `skipped` | `failed` | `pending`
    pub outcome: String,
    /// Stable code for skipped rows (see `Skip::code`).
    pub reason: String,
    /// Plain-English explanation.
    pub detail: String,
}

fn log_wire(r: ReplyLog) -> ReplyLogWire {
    ReplyLogWire { id: r.id, account: r.account, ts: r.ts, sender: r.sender, subject: r.subject, outcome: r.outcome, reason: r.reason, detail: r.detail }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewWire {
    /// Whole message as it would go out, for a made-up sender. NOTHING is sent.
    pub raw: String,
    pub to: String,
    pub subject: String,
    pub body: String,
}

fn run_state(states: &[proposals::State]) -> &'static str {
    use proposals::State::*;
    if states.is_empty() {
        "done"
    } else if states.iter().all(|s| *s == Undone) {
        "undone"
    } else if states.iter().all(|s| *s == Failed) {
        "failed"
    } else if states.iter().any(|s| matches!(s, Failed | PartiallyDone)) {
        "partial"
    } else {
        "done"
    }
}

impl Automation {
    pub fn list_rules(&self) -> R<Vec<RuleWire>> {
        Ok(lock(&self.store).list_rules(None).map_err(db)?.iter().map(rule_wire).collect())
    }

    pub fn save_rule(&self, input: RuleInput) -> R<RuleWire> {
        let now = (self.now)();
        let id = {
            let mut st = lock(&self.store);
            if let Some(id) = input.id {
                let existing = st.get_rule(id).map_err(db)?.ok_or("That rule no longer exists.")?;
                if existing.account != input.account {
                    return Err("A rule cannot be moved to another account.".into());
                }
                st.update_rule(id, &input.name, &input.spec, now)?;
                id
            } else {
                // Rules typed by hand start OFF too: the user reviews the preview, then turns them on.
                st.create_rule(&rules::NewRule { account: input.account, name: input.name, spec: input.spec, origin: "user", conv_id: None, reason: String::new(), enabled: false }, now)?
            }
        };
        let r = lock(&self.store).get_rule(id).map_err(db)?.ok_or("That rule no longer exists.")?;
        Ok(rule_wire(&r))
    }

    pub fn set_rule_enabled(&self, id: i64, enabled: bool) -> R<RuleWire> {
        let now = (self.now)();
        let rule = lock(&self.store).get_rule(id).map_err(db)?.ok_or("That rule no longer exists.")?;
        if enabled && rule.spec.is_none() {
            return Err("This rule is damaged. Edit and save it first.".into());
        }
        if enabled {
            // Only mail that arrives from now on is processed; the preview showed what existing mail would match.
            // The cursors are set BEFORE the rule is switched on, so it never runs without them.
            self.runner().baseline(id)?;
        }
        lock(&self.store).set_rule_enabled(id, enabled, now).map_err(db)?;
        let r = lock(&self.store).get_rule(id).map_err(db)?.ok_or("That rule no longer exists.")?;
        Ok(rule_wire(&r))
    }

    pub fn delete_rule(&self, id: i64) -> R<()> {
        lock(&self.store).delete_rule(id).map_err(db)?;
        Ok(())
    }

    pub fn rule_card(&self, id: i64) -> R<RuleCardWire> {
        let r = lock(&self.store).get_rule(id).map_err(db)?.ok_or("That rule no longer exists.")?;
        let dry_run = match &r.spec {
            Some(spec) => Some(dry_wire(self.runner().dry_run(&r.account, spec)?)),
            None => None,
        };
        Ok(RuleCardWire { rule: rule_wire(&r), dry_run })
    }

    pub fn dry_run(&self, account: &str, spec: &RuleSpec) -> R<DryRunWire> {
        let spec = rules::validate(spec)?;
        Ok(dry_wire(self.runner().dry_run(account, &spec)?))
    }

    /// "Apply to existing messages": how many cached messages the rule would change right now (nothing is changed).
    pub fn preview_existing(&self, id: i64) -> R<DryRunWire> {
        let r = lock(&self.store).get_rule(id).map_err(db)?.ok_or("That rule no longer exists.")?;
        let spec = r.spec.ok_or("This rule is damaged. Edit and save it first.")?;
        let mut d = self.runner().preview_existing(&r.account, &spec, (self.now)())?;
        let token = rules::targets_token(id, &d.targets);
        d.token = Some(token.clone());
        self.previews.lock().unwrap_or_else(|p| p.into_inner()).insert(id, (token, std::mem::take(&mut d.targets)));
        Ok(dry_wire(d))
    }

    /// Run the rule once over the cached messages that match; each folder becomes one run that can be undone.
    /// With a `token` (from the preview the user confirmed) only that preview's messages are changed.
    pub async fn apply_existing(&self, id: i64, token: Option<String>) -> R<(Vec<RunWire>, Vec<(String, String)>)> {
        let account = lock(&self.store).get_rule(id).map_err(db)?.ok_or("That rule no longer exists.")?.account;
        let lk = self.account_lock(&account);
        let _g = lk.lock().await;
        let reports = match token {
            Some(t) => {
                let targets = match self.previews.lock().unwrap_or_else(|p| p.into_inner()).get(&id) {
                    Some((pt, targets)) if *pt == t => targets.clone(),
                    _ => return Err("The preview is out of date. Preview again, then apply.".into()),
                };
                self.runner().apply_targets(id, &targets, &t, (self.now)()).await?
            }
            None => self.runner().apply_existing(id, (self.now)()).await?,
        };
        self.previews.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        let mut affected: Vec<(String, String)> = reports.iter().flat_map(|r| r.affected.iter().cloned()).collect();
        affected.sort();
        affected.dedup();
        let ids: std::collections::HashSet<i64> = reports.iter().map(|r| r.run_id).collect();
        let runs = self.rule_runs(200)?.into_iter().filter(|r| ids.contains(&r.id)).collect();
        Ok((runs, affected))
    }

    pub fn state(&self) -> R<AutomationState> {
        let st = lock(&self.store);
        Ok(AutomationState { rules_paused: st.rules_paused().map_err(db)?, autoreply_paused: st.autoreply_paused().map_err(db)? })
    }

    pub fn set_rules_paused(&self, paused: bool) -> R<AutomationState> {
        lock(&self.store).set_rules_paused(paused).map_err(db)?;
        self.state()
    }

    pub fn set_autoreply_paused(&self, paused: bool) -> R<AutomationState> {
        lock(&self.store).set_autoreply_paused(paused).map_err(db)?;
        self.state()
    }

    pub fn rule_runs(&self, limit: u32) -> R<Vec<RunWire>> {
        let mut st = lock(&self.store);
        let runs = st.list_rule_runs(limit.clamp(1, 200)).map_err(db)?;
        let mut out = Vec::new();
        for r in runs {
            let mut states = Vec::new();
            let mut undoable = false;
            for pid in &r.proposal_ids {
                if let Some(p) = st.get_proposal(*pid).map_err(db)? {
                    let log = st.list_log(*pid).map_err(db)?;
                    undoable |= proposals::undoable(&p, &log);
                    states.push(p.state);
                }
            }
            // Colour labels are local and have no proposal: they are logged per run.
            let (colours, colours_pending) = st.colour_log_counts(r.id).map_err(db)?;
            undoable |= colours_pending > 0;
            let mut state = run_state(&states);
            if colours > 0 {
                let procs_undone = !states.is_empty() && state == "undone";
                state = match (states.is_empty() || procs_undone, colours_pending) {
                    (true, 0) => "undone",
                    (true, n) if n < colours => "partial",
                    (true, _) => "done",
                    (false, _) => state,
                };
                if procs_undone && colours_pending > 0 {
                    state = "partial";
                }
            }
            out.push(RunWire { id: r.id, rule_id: r.rule_id, rule_name: r.rule_name, account: r.account, folder: r.folder, ts: r.ts, matched: r.matched, summary: r.summary, state: state.into(), undoable });
        }
        Ok(out)
    }

    pub async fn undo_run(&self, id: i64) -> R<(UndoRunWire, Vec<(String, String)>)> {
        let u = self.runner().undo_run(id).await?;
        Ok((UndoRunWire { undone: u.undone, failed: u.failed, errors: u.errors }, u.affected))
    }

    pub fn autoreply_get(&self, account: &str) -> R<AutoReplyConfig> {
        lock(&self.store).autoreply_config(account).map_err(db)
    }

    pub fn autoreply_set(&self, account: &str, cfg: AutoReplyConfig) -> R<AutoReplyConfig> {
        // The account text becomes a database key and the From address: it must be one plain address, and exactly
        // one of the configured accounts (no look-alike or alias spelling).
        if rules::addresses_of(account).len() != 1 || account.len() > 254 || !self.inbox.knows_account(account) {
            return Err("That is not one of your mail accounts.".into());
        }
        let cfg = autoreply::validate_config(&cfg)?;
        let was = lock(&self.store).autoreply_config(account).map_err(db)?.enabled;
        lock(&self.store).set_autoreply_config(account, &cfg).map_err(db)?;
        if cfg.enabled && !was {
            // Turning it on never answers mail that is already there.
            if let Some(v) = self.inbox.validity(account)? {
                let max = self.inbox.max_uid(account)?.unwrap_or(0);
                lock(&self.store).set_autoreply_cursor(account, v, max).map_err(db)?;
            }
        }
        Ok(cfg)
    }

    /// Render the reply for a made-up sender WITHOUT sending anything.
    pub fn autoreply_preview(&self, account: &str, cfg: &AutoReplyConfig, original_subject: &str) -> R<PreviewWire> {
        let mut c = cfg.clone();
        c.enabled = false; // validation of the text only; the switch is irrelevant to a preview
        let c = autoreply::validate_config(&c)?;
        if c.body.is_empty() {
            return Err("Type the reply text first.".into());
        }
        let (subject, body) = autoreply::render(&c, original_subject);
        let plan = Plan {
            to: "sender@example.com".into(),
            from_addr: "sender@example.com".into(),
            subject: subject.clone(),
            body: body.clone(),
            in_reply_to: "original-message@example.com".into(),
            references: vec!["original-message@example.com".into()],
            headers: vec![("Auto-Submitted".into(), "auto-replied".into())],
        };
        let msg = mailcore::smtp::build_message(account, &reply_message(&plan)).map_err(|e| e.to_string())?;
        Ok(PreviewWire { raw: String::from_utf8_lossy(&msg.formatted()).into_owned(), to: plan.to, subject, body })
    }

    pub fn autoreply_log(&self, account: Option<&str>, limit: u32) -> R<Vec<ReplyLogWire>> {
        Ok(lock(&self.store).list_autoreply_log(account, limit.clamp(1, 500)).map_err(db)?.into_iter().map(log_wire).collect())
    }
}

#[tauri::command]
pub async fn ai_rules_list(auto: State<'_, Automation>) -> R<Vec<RuleWire>> {
    auto.list_rules()
}

#[tauri::command]
pub async fn ai_rule_save(auto: State<'_, Automation>, rule: RuleInput) -> R<RuleWire> {
    auto.save_rule(rule)
}

#[tauri::command]
pub async fn ai_rule_set_enabled(auto: State<'_, Automation>, id: i64, enabled: bool) -> R<RuleWire> {
    auto.set_rule_enabled(id, enabled)
}

#[tauri::command]
pub async fn ai_rule_delete(auto: State<'_, Automation>, id: i64) -> R<()> {
    auto.delete_rule(id)
}

#[tauri::command]
pub async fn ai_rule_card(auto: State<'_, Automation>, id: i64) -> R<RuleCardWire> {
    auto.rule_card(id)
}

#[tauri::command]
pub async fn ai_rule_dry_run(auto: State<'_, Automation>, account: String, spec: RuleSpec) -> R<DryRunWire> {
    auto.dry_run(&account, &spec)
}

#[tauri::command]
pub async fn ai_rule_preview_existing(auto: State<'_, Automation>, id: i64) -> R<DryRunWire> {
    auto.preview_existing(id)
}

#[tauri::command]
pub async fn ai_rule_apply_existing(app: AppHandle, auto: State<'_, Automation>, id: i64, token: Option<String>) -> R<Vec<RunWire>> {
    let (runs, affected) = auto.apply_existing(id, token).await?;
    emit_changed(&app, &affected);
    Ok(runs)
}

#[tauri::command]
pub async fn ai_automation_state(auto: State<'_, Automation>) -> R<AutomationState> {
    auto.state()
}

#[tauri::command]
pub async fn ai_rules_pause(auto: State<'_, Automation>, paused: bool) -> R<AutomationState> {
    auto.set_rules_paused(paused)
}

#[tauri::command]
pub async fn ai_autoreply_pause(auto: State<'_, Automation>, paused: bool) -> R<AutomationState> {
    auto.set_autoreply_paused(paused)
}

#[tauri::command]
pub async fn ai_rule_runs(auto: State<'_, Automation>, limit: Option<u32>) -> R<Vec<RunWire>> {
    auto.rule_runs(limit.unwrap_or(50))
}

#[tauri::command]
pub async fn ai_rule_run_undo(app: AppHandle, auto: State<'_, Automation>, run_id: i64) -> R<UndoRunWire> {
    let (res, affected) = auto.undo_run(run_id).await?;
    emit_changed(&app, &affected);
    Ok(res)
}

#[tauri::command]
pub async fn ai_autoreply_get(auto: State<'_, Automation>, account: String) -> R<AutoReplyConfig> {
    auto.autoreply_get(&account)
}

#[tauri::command]
pub async fn ai_autoreply_set(auto: State<'_, Automation>, account: String, config: AutoReplyConfig) -> R<AutoReplyConfig> {
    auto.autoreply_set(&account, config)
}

#[tauri::command]
pub async fn ai_autoreply_preview(auto: State<'_, Automation>, account: String, config: AutoReplyConfig, subject: Option<String>) -> R<PreviewWire> {
    auto.autoreply_preview(&account, &config, subject.as_deref().unwrap_or("Your question"))
}

#[tauri::command]
pub async fn ai_autoreply_log(auto: State<'_, Automation>, account: Option<String>, limit: Option<u32>) -> R<Vec<ReplyLogWire>> {
    auto.autoreply_log(account.as_deref(), limit.unwrap_or(100))
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests;
