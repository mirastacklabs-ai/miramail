//! Read-only mail tools for the agent loop (AI chat P1).
//!
//! `aichat` does not know about mailcore. The app implements [`MailToolHost`] over its mail store;
//! this module owns everything that must not depend on the app being careful:
//!
//! * the model only ever sees conversation-local references (`a:1`, `f:2`, `m:12`), never raw
//!   account / folder / UID values; references are persisted per conversation in `conv_refs`;
//! * on EVERY resolve the reference's folder must still be in the scope the user ticked in the
//!   consent dialog and the folder's UIDVALIDITY must still equal the one recorded when the
//!   reference was minted (a reset folder means the UID may name another message);
//! * results returned by the host are filtered to the scope again, sanitised ([`crate::envelope`]),
//!   redacted ([`crate::redact`]), and capped (60K untrusted chars per turn, 20 calls per turn);
//! * there are no write operations anywhere in the trait. The `propose_*` tools (P2) only store a
//!   pending proposal ([`crate::proposals`]); the app, not the model, writes its title and warnings,
//!   and only the user's approval in the UI ever reaches a mail server.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

mod cal;

use crate::cal::CalendarHost;
use crate::drafts::{self, DraftHost, DraftKind};
use crate::envelope::{self, clean_line};
use crate::proposals::{self, FlagOp, Kind, MailActionHost, NewItem, NewProposal, Params, RoleFolder};
use crate::redact::{redact_body, redact_field, AddrMasker, RedactOpts};
use crate::registry::Registry;
use crate::store::{ConvRef, Store};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Max untrusted characters returned to the model in one user turn.
pub const MAX_UNTRUSTED_CHARS: usize = 60_000;
/// Max tool calls executed in one user turn.
pub const MAX_CALLS_PER_TURN: usize = 20;
/// Max rounds in which the model may ask for tools in one turn.
pub const MAX_TOOL_ROUNDS: usize = 8;

const DEFAULT_BODY_CHARS: usize = 6000;
const THREAD_EXCERPT_CHARS: usize = 1200;
/// `list_rules` shows at most this many rules per call, each description clipped to [`RULE_DESC_CHARS`].
pub const MAX_RULES_LISTED: usize = 50;
const RULE_DESC_CHARS: usize = 600;

/// A folder the user allowed in the consent dialog.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    pub account: String,
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgMeta {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub subject: String,
    pub from: String,
    pub to: String,
    /// Unix seconds.
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub snippet: String,
    /// Looks like a newsletter / automated message.
    pub bulk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMeta {
    pub name: String,
    pub role: String,
    pub total: u32,
    pub unread: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOut {
    pub hits: Vec<MsgMeta>,
    pub total: u32,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSearchArgs {
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub text: Option<String>,
    pub since: Option<String>,
    pub before: Option<String>,
    pub unseen: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderStat {
    pub account: String,
    pub folder: String,
    pub total: u32,
    pub unread: u32,
    pub flagged: u32,
    pub oldest: i64,
    pub newest: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsOut {
    pub folders: Vec<FolderStat>,
    pub top_senders: Vec<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgText {
    pub meta: MsgMeta,
    pub body: String,
    pub list_id: String,
    pub reply_to: String,
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadItem {
    pub meta: MsgMeta,
    /// Body text if it is cached locally.
    pub body: Option<String>,
}

/// Read access to the user's mail, implemented by the app. All methods are read-only by
/// construction. The engine passes only coordinates it has already scope- and validity-checked; a
/// host should still treat `scopes` as a hard limit. Futures cannot borrow the arguments (clone
/// what you need before `Box::pin(async move { .. })`). Error strings must be safe to show the
/// model and must not contain mail content.
pub trait MailToolHost: Send + Sync {
    fn accounts(&self) -> BoxFut<'_, Result<Vec<String>, String>>;
    fn folders(&self, account: &str) -> BoxFut<'_, Result<Vec<FolderMeta>, String>>;
    /// Current UIDVALIDITY of a folder as known locally (`None` = never synced).
    fn folder_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String>;
    /// Local full-text search restricted to `scopes`.
    fn search(&self, scopes: &[Scope], query: &str, limit: u32, offset: u32) -> BoxFut<'_, Result<SearchOut, String>>;
    /// Server-side search in one folder; hits are cached locally so they can be read afterwards.
    fn server_search(&self, scope: &Scope, expected_validity: Option<u32>, q: &ServerSearchArgs, limit: u32) -> BoxFut<'_, Result<Vec<MsgMeta>, String>>;
    fn stats(&self, scopes: &[Scope]) -> BoxFut<'_, Result<StatsOut, String>>;
    fn get_message(&self, scope: &Scope, uid: u32, uidvalidity: u32) -> BoxFut<'_, Result<MsgText, String>>;
    /// Thread of `(scope, uid)` restricted to `scopes`, oldest first.
    fn get_thread(&self, scopes: &[Scope], scope: &Scope, uid: u32, uidvalidity: u32, limit: u32) -> BoxFut<'_, Result<Vec<ThreadItem>, String>>;
}

/// Calendar access for one conversation (present only when the user ticked "Let MIRA see my calendar").
#[derive(Clone)]
pub struct CalSetup {
    pub host: Arc<dyn CalendarHost>,
    /// Calendar ids the user allowed. Empty = the calendar tools see nothing.
    pub calendars: Vec<String>,
    /// The user allowed "Assistant may suggest changes": the calendar `propose_*` tools are offered.
    pub propose: bool,
}

/// Everything the engine needs to offer tools in one conversation.
#[derive(Clone)]
pub struct ToolSetup {
    pub host: Arc<dyn MailToolHost>,
    /// Folders the user allowed. Empty = tools can see nothing.
    pub scopes: Vec<Scope>,
    pub redaction: RedactOpts,
    /// `Some` = the user allowed "Assistant may suggest changes": the `propose_*` tools are offered.
    /// The model never touches this; it only lets proposals look messages up.
    pub actions: Option<Arc<dyn MailActionHost>>,
    /// `Some` = the user allowed "Assistant may suggest changes": `draft_reply` / `draft_new` are offered. The host
    /// only reads mail; a draft is stored for the user to open in Compose and send themselves.
    pub drafts: Option<Arc<dyn DraftHost>>,
    /// `Some` = the calendar read tools (and, with `propose`, the calendar proposal tools) are offered.
    pub calendar: Option<CalSetup>,
}

/// One executed (or refused) tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    /// Text sent back to the model.
    pub content: String,
    pub ok: bool,
    /// Short description of the call for the UI chip (from validated arguments).
    pub label: String,
    /// Short outcome for the UI chip ("12 results"). Never mail text.
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EgressItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub subject: String,
    pub from: String,
}

pub struct ToolRuntime {
    setup: ToolSetup,
    registry: Registry,
    store: Arc<Mutex<Store>>,
    conv_id: i64,
    nonce: String,
    refs: Vec<ConvRef>,
    allowed: HashSet<Scope>,
    /// Calendar ids the consent allows.
    cal_allowed: HashSet<String>,
    used_chars: usize,
    char_budget: usize,
    ref_failed: bool,
    replayed: usize,
    calls: usize,
    items: Vec<EgressItem>,
    masker: AddrMasker,
    /// Proposals queued during this turn: `(id, title)`; drained by the engine to notify the UI.
    new_proposals: Vec<(i64, String)>,
    proposals_made: usize,
    /// Drafts written during this turn: `(id, title)`; drained by the engine to notify the UI.
    new_drafts: Vec<(i64, String)>,
    drafts_made: usize,
    /// Rules proposed during this turn: `(id, title)`. They are stored DISABLED.
    new_rules: Vec<(i64, String)>,
    rules_made: usize,
}

fn lock(s: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

type Res<T> = Result<T, String>;

impl ToolRuntime {
    /// Load the conversation's references and start a new turn (fresh nonce, fresh caps).
    pub fn new(mut setup: ToolSetup, store: Arc<Mutex<Store>>, conv_id: i64) -> Self {
        let refs = lock(&store).list_refs(conv_id).unwrap_or_default();
        // Folders created in this conversation through an approved create-folder card are in scope too, but only if
        // their account is still ticked (the consent is the ceiling).
        let created = lock(&store).list_conv_folders(conv_id).unwrap_or_default();
        for (account, folder) in created {
            let s = Scope { account, folder };
            if setup.scopes.iter().any(|x| x.account == s.account) && !setup.scopes.contains(&s) {
                setup.scopes.push(s);
            }
        }
        let allowed = setup.scopes.iter().cloned().collect();
        let mut registry = if setup.actions.is_some() { Registry::mail_read().with_proposals() } else { Registry::mail_read() };
        if setup.actions.is_some() {
            // Rules are reachable only in conversations where the user allowed suggestions (same gate as propose_rule).
            registry = registry.with_rules_read();
        }
        if setup.drafts.is_some() {
            registry = registry.with_drafts();
        }
        let cal_allowed: HashSet<String> = setup.calendar.as_ref().map(|c| c.calendars.iter().cloned().collect()).unwrap_or_default();
        if let Some(c) = &setup.calendar {
            registry = registry.with_calendar_read();
            if c.propose {
                registry = registry.with_calendar_propose();
            }
        }
        ToolRuntime {
            setup,
            registry,
            store,
            conv_id,
            nonce: envelope::new_nonce(),
            refs,
            allowed,
            cal_allowed,
            used_chars: 0,
            char_budget: MAX_UNTRUSTED_CHARS,
            ref_failed: false,
            replayed: 0,
            calls: 0,
            items: Vec::new(),
            masker: AddrMasker::default(),
            new_proposals: Vec::new(),
            proposals_made: 0,
            new_drafts: Vec::new(),
            drafts_made: 0,
            new_rules: Vec::new(),
            rules_made: 0,
        }
    }

    /// Rules proposed since the last call (id, app-written title).
    pub fn take_new_rules(&mut self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.new_rules)
    }

    /// Drafts written since the last call (id, app-written title).
    pub fn take_new_drafts(&mut self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.new_drafts)
    }

    /// Proposals created since the last call (id, app-written title).
    pub fn take_new_proposals(&mut self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.new_proposals)
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Mail items whose text has been placed in tool results so far this turn (for the egress log).
    pub fn items_json(&self) -> String {
        let mut items = self.items.clone();
        if self.replayed > 0 {
            // Tool results from earlier turns are sent again as part of the history.
            items.insert(0, EgressItem { reference: "earlier".into(), subject: format!("includes {} earlier mail excerpt{} from this conversation", self.replayed, if self.replayed == 1 { "" } else { "s" }), from: String::new() });
        }
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
    }

    /// Number of tool results from earlier turns that are replayed in this turn's requests.
    pub fn set_replayed(&mut self, n: usize) {
        self.replayed = n;
    }

    // ---- references ---------------------------------------------------------------------------

    fn mint(&mut self, kind: char, account: &str, folder: &str, uid: u32, validity: u32) -> String {
        let prefix = format!("{kind}:");
        if let Some(r) = self.refs.iter().find(|r| {
            r.reference.starts_with(&prefix) && r.account == account && r.folder == folder && r.uid == i64::from(uid) && r.uidvalidity == i64::from(validity)
        }) {
            return r.reference.clone();
        }
        let next = self.refs.iter().filter_map(|r| r.reference.strip_prefix(&prefix)?.parse::<u64>().ok()).max().unwrap_or(0) + 1;
        let cr = ConvRef { reference: format!("{prefix}{next}"), account: account.into(), folder: folder.into(), uid: i64::from(uid), uidvalidity: i64::from(validity) };
        if lock(&self.store).put_ref(self.conv_id, &cr).is_err() {
            self.ref_failed = true; // execute() turns this into an error result for the call
        }
        let name = cr.reference.clone();
        self.refs.push(cr);
        name
    }

    fn lookup(&self, kind: char, r: &str) -> Res<&ConvRef> {
        let ok_shape = r.len() <= 16 && r.starts_with(kind) && r[1..].starts_with(':') && r.len() > 2 && r[2..].bytes().all(|b| b.is_ascii_digit());
        if !ok_shape {
            return Err(format!("\"{}\" is not a valid {} reference. Use a reference from a tool result.", clean_line(r, 20), kind_name(kind)));
        }
        self.refs
            .iter()
            .find(|c| c.reference == r)
            .ok_or_else(|| format!("Unknown reference {r}. Use references from the results of earlier tool calls."))
    }

    fn in_scope(&self, account: &str, folder: &str) -> bool {
        self.allowed.contains(&Scope { account: account.into(), folder: folder.into() })
    }

    fn resolve_account(&self, r: &str) -> Res<String> {
        let c = self.lookup('a', r)?;
        if !self.setup.scopes.iter().any(|s| s.account == c.account) {
            return Err(format!("{r} is outside the folders the user allowed for this conversation."));
        }
        Ok(c.account.clone())
    }

    fn resolve_folder(&self, r: &str) -> Res<Scope> {
        let c = self.lookup('f', r)?;
        if !self.in_scope(&c.account, &c.folder) {
            return Err(format!("{r} is outside the folders the user allowed for this conversation."));
        }
        Ok(Scope { account: c.account.clone(), folder: c.folder.clone() })
    }

    /// Scope AND UIDVALIDITY are rechecked here, on every call.
    fn resolve_message(&self, r: &str) -> Res<(Scope, u32, u32)> {
        let c = self.lookup('m', r)?;
        if !self.in_scope(&c.account, &c.folder) {
            return Err(format!("{r} is outside the folders the user allowed for this conversation."));
        }
        match self.setup.host.folder_validity(&c.account, &c.folder)? {
            Some(v) if i64::from(v) == c.uidvalidity => {}
            _ => return Err(format!("{r} is no longer valid because the mailbox was reset on the server. Search again to get a fresh reference.")),
        }
        let uid = u32::try_from(c.uid).map_err(|_| "Invalid reference.".to_string())?;
        let validity = u32::try_from(c.uidvalidity).map_err(|_| "Invalid reference.".to_string())?;
        Ok((Scope { account: c.account.clone(), folder: c.folder.clone() }, uid, validity))
    }

    // ---- budget -------------------------------------------------------------------------------

    /// Lower this turn's allowance of untrusted chars (never raises it above [`MAX_UNTRUSTED_CHARS`]).
    pub fn set_char_budget(&mut self, chars: usize) {
        self.char_budget = chars.min(MAX_UNTRUSTED_CHARS);
    }

    fn remaining(&self) -> usize {
        self.char_budget.saturating_sub(self.used_chars)
    }

    /// Reserve `n` untrusted chars; false when the turn's allowance is exhausted.
    fn reserve(&mut self, n: usize) -> bool {
        if n > self.remaining() {
            return false;
        }
        self.used_chars += n;
        true
    }

    // ---- execution ----------------------------------------------------------------------------

    /// Validate and run one call. Never panics; every failure is an error outcome.
    pub async fn execute(&mut self, name: &str, args: &Value) -> ToolOutcome {
        let label = label_for(name, args);
        self.calls += 1;
        if self.calls > MAX_CALLS_PER_TURN {
            return fail(label, format!("Tool call limit reached ({MAX_CALLS_PER_TURN} per question). Answer with what you have."), "limit reached");
        }
        if let Err(e) = self.registry.validate_call(name, args) {
            return fail(label, e, "invalid call");
        }
        self.ref_failed = false;
        let r = match name {
            "list_accounts" => self.t_list_accounts().await,
            "list_folders" => self.t_list_folders(args).await,
            "search_mail" => self.t_search(args).await,
            "server_search" => self.t_server_search(args).await,
            "mail_stats" => self.t_stats(args).await,
            "get_message" => self.t_get_message(args).await,
            "get_thread" => self.t_get_thread(args).await,
            "propose_move" | "propose_archive" | "propose_trash" | "propose_flag" | "propose_create_folder" | "propose_rule_change" => self.t_propose(name, args),
            "propose_rule" => self.t_propose_rule(args),
            "list_rules" => self.t_list_rules(args),
            "draft_reply" | "draft_new" => self.t_draft(name, args).await,
            "list_calendars" => self.t_list_calendars(),
            "list_events" => self.t_list_events(args),
            "get_event" => self.t_get_event(args),
            "free_busy" => self.t_free_busy(args),
            "find_free_slots" => self.t_find_free_slots(args),
            "propose_event" => self.t_propose_event(args),
            "propose_event_change" => self.t_propose_event_change(args),
            "propose_event_cancel" => self.t_propose_event_cancel(args),
            "propose_invite_response" => self.t_propose_invite_response(args).await,
            _ => Err("Unknown tool.".to_string()),
        };
        if self.ref_failed {
            // A reference the model was given could not be saved, so it would not resolve next turn.
            return fail(label, "MiraMail could not save the message references. Try again.".into(), "failed");
        }
        match r {
            Ok((content, summary)) => ToolOutcome { content, ok: true, label, summary },
            Err(e) => fail(label, clean_line(&e, 300), "failed"),
        }
    }

    fn scopes_for(&self, folder_arg: Option<&Value>) -> Res<Vec<Scope>> {
        match folder_arg.and_then(Value::as_str) {
            Some(f) => Ok(vec![self.resolve_folder(f)?]),
            None => Ok(self.setup.scopes.clone()),
        }
    }

    async fn t_list_accounts(&mut self) -> Res<(String, String)> {
        let mine: Vec<String> = self.setup.host.accounts().await?;
        let mut seen = HashSet::new();
        let mut lines = Vec::new();
        let scopes = self.setup.scopes.clone();
        for s in &scopes {
            if seen.insert(s.account.clone()) && mine.contains(&s.account) {
                let r = self.mint('a', &s.account, "", 0, 0);
                let shown = redact_field(&s.account, &self.setup.redaction, &mut self.masker);
                lines.push(format!("{r}: {shown}"));
            }
        }
        let n = lines.len();
        let body = if lines.is_empty() { "No accounts are available in this conversation.".to_string() } else { lines.join("\n") };
        Ok((format!("{n} account(s):\n{body}"), plural(n, "account")))
    }

    async fn t_list_folders(&mut self, args: &Value) -> Res<(String, String)> {
        let accounts: Vec<String> = match args.get("account").and_then(Value::as_str) {
            Some(a) => vec![self.resolve_account(a)?],
            None => {
                let mut v: Vec<String> = Vec::new();
                for s in &self.setup.scopes {
                    if !v.contains(&s.account) {
                        v.push(s.account.clone());
                    }
                }
                v
            }
        };
        let mut lines = Vec::new();
        for acc in accounts {
            let aref = self.mint('a', &acc, "", 0, 0);
            for f in self.setup.host.folders(&acc).await? {
                if !self.in_scope(&acc, &f.name) {
                    continue;
                }
                let fref = self.mint('f', &acc, &f.name, 0, 0);
                lines.push(format!("{fref} (account {aref}) {} [{}]: {} messages, {} unread", clean_line(&f.name, 100), clean_line(&f.role, 16), f.total, f.unread));
            }
        }
        let n = lines.len();
        if n == 0 {
            return Ok(("No readable folders.".into(), plural(0, "folder")));
        }
        let block = self.untrusted("folders", &lines.join("\n"), usize::MAX).ok_or_else(budget_msg)?;
        Ok((format!("{n} folder(s). Folder names come from the mail server.\n{block}"), plural(n, "folder")))
    }

    /// Wrap + count. `max` caps the body chars taken from `text`.
    fn untrusted(&mut self, label: &str, text: &str, max: usize) -> Option<String> {
        let mut body: String = envelope::clean_text(text);
        if body.chars().count() > max {
            body = body.chars().take(max).collect();
        }
        let wrapped = envelope::wrap(&self.nonce, label, &body);
        let n = wrapped.chars().count();
        if !self.reserve(n) {
            return None;
        }
        Some(wrapped)
    }

    fn message_header(&mut self, m: &MsgMeta, folder_ref: &str) -> String {
        let red = self.setup.redaction;
        let from = redact_field(&clean_line(&m.from, 200), &red, &mut self.masker);
        let to = redact_field(&clean_line(&m.to, 300), &red, &mut self.masker);
        let subject = redact_field(&clean_line(&m.subject, 200), &red, &mut self.masker);
        let mut flags = vec![if m.seen { "read" } else { "unread" }];
        if m.flagged {
            flags.push("flagged");
        }
        if m.answered {
            flags.push("answered");
        }
        if m.bulk {
            flags.push("newsletter-or-automated");
        }
        format!("Folder: {folder_ref} {}\nDate: {}\nStatus: {}\nFrom: {from}\nTo: {to}\nSubject: {subject}", clean_line(&m.folder, 100), fmt_ts(m.date), flags.join(", "))
    }

    fn note_item(&mut self, mref: &str, m: &MsgMeta) {
        if self.items.iter().any(|i| i.reference == mref) {
            return;
        }
        let red = self.setup.redaction;
        let subject = redact_field(&clean_line(&m.subject, 120), &red, &mut self.masker);
        let from = redact_field(&clean_line(&m.from, 120), &red, &mut self.masker);
        self.items.push(EgressItem { reference: mref.to_string(), subject, from });
    }

    fn mint_message(&mut self, m: &MsgMeta) -> Option<(String, String)> {
        // Defence in depth: a host bug must not leak an out-of-scope message.
        if !self.in_scope(&m.account, &m.folder) {
            return None;
        }
        let fref = self.mint('f', &m.account, &m.folder, 0, 0);
        let mref = self.mint('m', &m.account, &m.folder, m.uid, m.uidvalidity);
        Some((mref, fref))
    }

    /// Render result rows as one envelope block per message, honouring the per-turn allowance.
    fn render_hits(&mut self, hits: &[MsgMeta]) -> (Vec<String>, usize, bool) {
        let mut blocks = Vec::new();
        let mut shown = 0;
        let mut truncated = false;
        for m in hits {
            let Some((mref, fref)) = self.mint_message(m) else { continue };
            let red = self.setup.redaction;
            let preview = redact_field(&clean_line(&m.snippet, 240), &red, &mut self.masker);
            let mut text = self.message_header(m, &fref);
            if !preview.is_empty() {
                text.push_str(&format!("\nPreview: {preview}"));
            }
            match self.untrusted(&mref, &text, usize::MAX) {
                Some(b) => {
                    self.note_item(&mref, m);
                    blocks.push(b);
                    shown += 1;
                }
                None => {
                    truncated = true;
                    break;
                }
            }
        }
        (blocks, shown, truncated)
    }

    async fn t_search(&mut self, args: &Value) -> Res<(String, String)> {
        let query = args["query"].as_str().unwrap_or_default().to_string();
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as u32;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as u32;
        let scopes = self.scopes_for(args.get("folder"))?;
        let out = self.setup.host.search(&scopes, &query, limit, offset).await?;
        let hits: Vec<MsgMeta> = out.hits.into_iter().filter(|m| scopes.iter().any(|s| s.account == m.account && s.folder == m.folder)).collect();
        let (blocks, shown, truncated) = self.render_hits(&hits);
        let mut head = format!("{} matching message(s) in the local copy; showing {shown} starting at {offset}.", out.total);
        for h in out.hints.iter().take(5) {
            head.push_str(&format!("\nNote: {}", clean_line(h, 120)));
        }
        if truncated {
            head.push_str("\nOutput cut short: the per-question limit for mail content was reached.");
        }
        if shown == 0 && !truncated {
            head.push_str("\nNothing found. The local copy may only hold recent mail; server_search can look further back in one folder.");
        }
        Ok((format!("{head}\n{}", blocks.join("\n")), plural_results(shown, out.total)))
    }

    async fn t_server_search(&mut self, args: &Value) -> Res<(String, String)> {
        let scope = self.resolve_folder(args["folder"].as_str().unwrap_or_default())?;
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let q = ServerSearchArgs {
            from: s("from"),
            to: s("to"),
            subject: s("subject"),
            text: s("text"),
            since: s("since"),
            before: s("before"),
            unseen: args.get("unseen").and_then(Value::as_bool),
        };
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as u32;
        let expected = self.setup.host.folder_validity(&scope.account, &scope.folder)?;
        let hits = self.setup.host.server_search(&scope, expected, &q, limit).await?;
        let hits: Vec<MsgMeta> = hits.into_iter().filter(|m| m.account == scope.account && m.folder == scope.folder).collect();
        let (blocks, shown, truncated) = self.render_hits(&hits);
        let mut head = format!("{shown} message(s) found on the server in that folder (newest first).");
        if truncated {
            head.push_str("\nOutput cut short: the per-question limit for mail content was reached.");
        }
        Ok((format!("{head}\n{}", blocks.join("\n")), plural_results(shown, shown as u32)))
    }

    async fn t_stats(&mut self, args: &Value) -> Res<(String, String)> {
        let scopes = self.scopes_for(args.get("folder"))?;
        let st = self.setup.host.stats(&scopes).await?;
        let mut lines = Vec::new();
        for f in st.folders.iter().filter(|f| scopes.iter().any(|s| s.account == f.account && s.folder == f.folder)) {
            let fref = self.mint('f', &f.account, &f.folder, 0, 0);
            lines.push(format!(
                "{fref} {}: {} messages, {} unread, {} flagged, oldest {}, newest {}",
                clean_line(&f.folder, 100),
                f.total,
                f.unread,
                f.flagged,
                if f.oldest > 0 { fmt_ts(f.oldest) } else { "-".into() },
                if f.newest > 0 { fmt_ts(f.newest) } else { "-".into() },
            ));
        }
        let n = lines.len();
        let red = self.setup.redaction;
        let senders: Vec<String> = st.top_senders.iter().take(10).map(|(s, c)| format!("{c} x {}", redact_field(&clean_line(s, 120), &red, &mut self.masker))).collect();
        let mut text = lines.join("\n");
        if !senders.is_empty() {
            text.push_str("\nMost frequent senders in the local copy:\n");
            text.push_str(&senders.join("\n"));
        }
        if text.is_empty() {
            return Ok(("No readable folders.".into(), plural(0, "folder")));
        }
        let block = self.untrusted("stats", &text, usize::MAX).ok_or_else(budget_msg)?;
        Ok((format!("Statistics for {n} folder(s) (counts cover the local copy).\n{block}"), plural(n, "folder")))
    }

    async fn t_get_message(&mut self, args: &Value) -> Res<(String, String)> {
        let r = args["ref"].as_str().unwrap_or_default();
        let (scope, uid, validity) = self.resolve_message(r)?;
        let max = args.get("max_chars").and_then(Value::as_u64).unwrap_or(DEFAULT_BODY_CHARS as u64) as usize;
        let msg = self.setup.host.get_message(&scope, uid, validity).await?;
        if msg.meta.account != scope.account || msg.meta.folder != scope.folder || msg.meta.uid != uid {
            return Err("The mail store returned a different message than requested.".into());
        }
        let fref = self.mint('f', &scope.account, &scope.folder, 0, 0);
        let red = self.setup.redaction;
        let body_all = redact_body(&envelope::strip_hidden(&msg.body), &red, &mut self.masker);
        let total = body_all.chars().count();
        let mut header = self.message_header(&msg.meta, &fref);
        if !msg.list_id.is_empty() {
            header.push_str(&format!("\nList-Id: {}", clean_line(&msg.list_id, 120)));
        }
        if !msg.reply_to.is_empty() {
            header.push_str(&format!("\nReply-To: {}", redact_field(&clean_line(&msg.reply_to, 200), &red, &mut self.masker)));
        }
        if !msg.attachments.is_empty() {
            let names: Vec<String> = msg.attachments.iter().take(10).map(|a| clean_line(a, 80)).collect();
            header.push_str(&format!("\nAttachments (names only, contents not shown): {}", names.join(", ")));
        }
        // Reserve for the wrapper/header first, then give the body whatever allowance is left.
        let overhead = envelope::OPEN.len() + envelope::CLOSE.len() + self.nonce.len() * 2 + r.len() + header.chars().count() + 24;
        let room = self.remaining().saturating_sub(overhead);
        if room < 200 {
            return Err(budget_msg());
        }
        let take = max.min(room);
        let body: String = body_all.chars().take(take).collect();
        let cut = total.saturating_sub(body.chars().count());
        let text = format!("{header}\n\n{body}");
        let block = self.untrusted(r, &text, usize::MAX).ok_or_else(budget_msg)?;
        self.note_item(r, &msg.meta);
        let note = if cut > 0 { format!("\n[Body truncated: {cut} more characters not shown.]") } else { String::new() };
        Ok((format!("Message {r}.{note}\n{block}"), if cut > 0 { "read (truncated)".into() } else { "read".into() }))
    }

    /// Queue a suggestion. Nothing here talks to a mail server; the only side effect is one row in ai.db.
    fn t_propose(&mut self, name: &str, args: &Value) -> Res<(String, String)> {
        let host = self.setup.actions.clone().ok_or_else(|| "Suggesting changes is not turned on for this conversation.".to_string())?;
        if self.proposals_made >= proposals::MAX_PER_TURN {
            return Err(format!("At most {} suggestions can be queued per question.", proposals::MAX_PER_TURN));
        }
        let reason = clean_line(args.get("reason").and_then(Value::as_str).unwrap_or(""), proposals::MAX_REASON_CHARS);
        let kind = match name {
            "propose_move" => Kind::Move,
            "propose_archive" => Kind::Archive,
            "propose_trash" => Kind::Trash,
            "propose_flag" => Kind::Flag,
            "propose_rule_change" => Kind::RuleChange,
            _ => Kind::CreateFolder,
        };
        let planned = match kind {
            Kind::CreateFolder => self.plan_create_folder(host.as_ref(), args)?,
            Kind::RuleChange => self.plan_rule_change(host.as_ref(), args)?,
            _ => self.plan_messages(host.as_ref(), kind, args)?,
        };
        let np = NewProposal { conv_id: self.conv_id, kind, params: planned.params.clone(), title: planned.title.clone(), reason, warnings: planned.warnings.clone(), items: planned.items.clone() };
        let now = crate::engine::system_clock()();
        let id = lock(&self.store).create_proposal(&np, now).map_err(|_| "MiraMail could not save the suggestion.".to_string())?;
        self.proposals_made += 1;
        self.new_proposals.push((id, planned.title.clone()));
        let mut msg = format!(
            "Suggestion #{id} is now waiting for the user's approval: {}. NOTHING has been changed; the user decides in MiraMail. Tell them a suggestion is waiting; do not say it was done.",
            planned.title
        );
        if kind == Kind::CreateFolder {
            msg.push_str(" Once the user approves, the new folder is available in this conversation: call list_folders on your next turn to get its reference, then it can be a move destination.");
        }
        if !planned.warnings.is_empty() {
            msg.push_str(&format!(" MiraMail's notes: {}.", planned.warnings.join("; ")));
        }
        Ok((msg, "waiting for approval".to_string()))
    }

    /// Read the user's existing rules for the accounts in this conversation's scope. Read-only; no message content.
    /// Names and condition text can contain mail-derived words, so the list travels inside the UNTRUSTED envelope.
    fn t_list_rules(&mut self, args: &Value) -> Res<(String, String)> {
        let only: Option<String> = match args.get("account").and_then(Value::as_str) {
            Some(a) => Some(self.resolve_account(a)?),
            None => None,
        };
        let mut accounts: Vec<String> = Vec::new();
        for s in &self.setup.scopes {
            if !accounts.contains(&s.account) && only.as_ref().is_none_or(|o| *o == s.account) {
                accounts.push(s.account.clone());
            }
        }
        let mut all = Vec::new();
        for acc in &accounts {
            let rules = lock(&self.store).list_rules(Some(acc)).map_err(|_| "MiraMail could not read the rules.".to_string())?;
            all.extend(rules);
        }
        let total = all.len();
        let mut lines = Vec::new();
        for r in all.into_iter().take(MAX_RULES_LISTED) {
            let aref = self.mint('a', &r.account, "", 0, 0);
            let what = match &r.spec {
                Some(spec) => clean_line(&crate::rules::describe(spec), RULE_DESC_CHARS),
                None => "(this rule could not be read)".to_string(),
            };
            let origin = if r.origin == "ai" { "ai" } else { "user" };
            lines.push(format!("rule {} \"{}\" (account {aref}, {}, made by {origin}): {what}", r.id, clean_line(&r.name, 100), if r.enabled { "on" } else { "off" }));
        }
        let shown = lines.len();
        if shown == 0 {
            return Ok(("No rules exist for the accounts in this conversation.".into(), plural(0, "rule")));
        }
        let more = if total > shown { format!(" ({} more not shown)", total - shown) } else { String::new() };
        let block = self.untrusted("rules", &lines.join("\n"), usize::MAX).ok_or_else(budget_msg)?;
        let summary = if total > shown { format!("{shown} of {total} rules") } else { plural(shown, "rule") };
        Ok((format!("{shown} rule(s){more}. Rule names and conditions are untrusted text.\n{block}"), summary))
    }

    /// Save a rule proposal. The rule is stored DISABLED (`enabled = false`, origin `ai`); nothing here talks to a
    /// mail server and nothing runs. The user sees an app-written description and a dry run, and turns it on.
    fn t_propose_rule(&mut self, args: &Value) -> Res<(String, String)> {
        use crate::rules::{self, Action, Condition, MatchMode, NewRule, RuleSpec};
        let host = self.setup.actions.clone().ok_or_else(|| "Suggesting changes is not turned on for this conversation.".to_string())?;
        if self.rules_made >= rules::MAX_PROPOSED_PER_TURN {
            return Err(format!("At most {} rules can be proposed per question.", rules::MAX_PROPOSED_PER_TURN));
        }
        let account = self.resolve_account(args["account"].as_str().unwrap_or_default())?;
        let name = rules::clean_rule_name(args["name"].as_str().unwrap_or_default())?;
        let reason = clean_line(args.get("reason").and_then(Value::as_str).unwrap_or(""), proposals::MAX_REASON_CHARS);
        let mode = if args.get("match").and_then(Value::as_str) == Some("any") { MatchMode::Any } else { MatchMode::All };
        let mut conditions = Vec::new();
        for c in args["conditions"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let cond: Condition = serde_json::from_value(c.clone()).map_err(|_| "A condition is not valid.".to_string())?;
            conditions.push(cond);
        }
        let mut actions = Vec::new();
        for a in args["actions"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let kind = a.get("type").and_then(Value::as_str).unwrap_or_default();
            actions.push(match kind {
                "archive" => Action::Archive,
                "trash" => Action::Trash,
                "markRead" => Action::MarkRead,
                "flag" => Action::Flag,
                "moveTo" => {
                    let raw = a.get("folder").and_then(Value::as_str).map(str::trim).filter(|f| !f.is_empty()).ok_or_else(|| "moveTo needs a folder.".to_string())?;
                    let folder = if raw.len() <= 16 && raw.starts_with("f:") {
                        let s = self.resolve_folder(raw)?;
                        if s.account != account {
                            return Err("The destination folder belongs to a different account.".into());
                        }
                        s.folder
                    } else {
                        host.find_folder(&account, raw)?.ok_or_else(|| "That destination folder does not exist. Suggest creating it first, or choose an existing folder.".to_string())?
                    };
                    Action::MoveTo { folder }
                }
                _ => return Err("Unknown action.".into()),
            });
        }
        let spec = rules::validate(&RuleSpec { v: rules::SPEC_VERSION, mode, conditions, actions })?;
        let now = crate::engine::system_clock()();
        let id = lock(&self.store).create_rule(&NewRule { account, name: name.clone(), spec, origin: "ai", conv_id: Some(self.conv_id), reason, enabled: false }, now)?;
        self.rules_made += 1;
        let title = format!("Rule (off): {name}");
        self.new_rules.push((id, title));
        Ok((
            format!("Rule #{id} was saved but is OFF. NOTHING has run and nothing will until the user reviews MiraMail's description and preview of it and turns it on. Tell them a rule is waiting; do not say it is active."),
            "saved off, waiting for review".to_string(),
        ))
    }

    /// Store a draft. Nothing here talks to a mail server and nothing can send: the recipients of a reply are
    /// computed from the stored message, the model contributes the body (cleaned) and its reason.
    async fn t_draft(&mut self, name: &str, args: &Value) -> Res<(String, String)> {
        let host = self.setup.drafts.clone().ok_or_else(|| "Drafting is not turned on for this conversation.".to_string())?;
        if self.drafts_made >= drafts::MAX_PER_TURN {
            return Err(format!("At most {} drafts can be written per question.", drafts::MAX_PER_TURN));
        }
        let reason = clean_line(args.get("reason").and_then(Value::as_str).unwrap_or(""), drafts::MAX_REASON_CHARS);
        let body = args["body"].as_str().unwrap_or_default().to_string();
        let planned = if name == "draft_reply" {
            let (scope, uid, validity) = self.resolve_message(args["ref"].as_str().unwrap_or_default())?;
            let src = host.reply_source(&scope, uid, validity).await?;
            let own = host.own_addresses(&scope.account)?;
            let reply_all = args.get("reply_all").and_then(Value::as_bool).unwrap_or(false);
            let known = |a: &str| host.address_known(a).unwrap_or(false);
            drafts::plan_reply(&scope.account, &src, &own, reply_all, &body, &known)?
        } else {
            let list = |k: &str| -> Vec<String> { args.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default() };
            // The sending account must be explicit when more than one account is in scope.
            let account = match args.get("account").and_then(Value::as_str).filter(|a| !a.trim().is_empty()) {
                Some(a) => self.resolve_account(a)?,
                None => {
                    let mut accounts: Vec<&str> = self.setup.scopes.iter().map(|s| s.account.as_str()).collect();
                    accounts.sort_unstable();
                    accounts.dedup();
                    match accounts.as_slice() {
                        [] => return Err("No account is available in this conversation.".into()),
                        [one] => (*one).to_string(),
                        _ => return Err("More than one account is available: pass \"account\" (a:1, ... from list_accounts) to say which one the message is sent from.".into()),
                    }
                }
            };
            let known = |a: &str| host.address_known(a).unwrap_or(false);
            drafts::plan_new(&account, &list("to"), &list("cc"), args["subject"].as_str().unwrap_or_default(), &body, &known)?
        };
        let now = crate::engine::system_clock()();
        let id = lock(&self.store).create_draft(self.conv_id, &planned, &reason, now).map_err(|_| "MiraMail could not save the draft.".to_string())?;
        self.drafts_made += 1;
        self.new_drafts.push((id, planned.title.clone()));
        // No addresses or subject lines go back to the model: with address masking on it must not learn them.
        let what = if planned.kind == DraftKind::Reply { "reply" } else { "new message" };
        let how = if planned.kind == DraftKind::Reply { "MiraMail addressed it from the original message" } else { "MiraMail checked the addresses" };
        let mut msg = format!(
            "Draft #{id} ({what}) is ready for the user to review. NOTHING has been sent and you cannot send mail; {how}. The user decides whether to open it in Compose and send it. Tell them a draft is waiting; do not say it was sent."
        );
        if !planned.warnings.is_empty() {
            msg.push_str(&format!(" MiraMail's notes (the user sees them): {} warning(s).", planned.warnings.len()));
        }
        Ok((msg, "draft ready for review".to_string()))
    }

    /// Turn an existing rule on/off or discard it. The rule must exist and belong to an account of this
    /// conversation's scope; the card is written from the STORED rule (name + the app's own description).
    fn plan_rule_change(&mut self, host: &dyn MailActionHost, args: &Value) -> Res<proposals::Planned> {
        use crate::rules;
        let account = self.resolve_account(args["account"].as_str().unwrap_or_default())?;
        let id = args.get("rule").and_then(Value::as_i64).ok_or_else(|| "rule must be the number shown by list_rules.".to_string())?;
        let change = args.get("change").and_then(Value::as_str).unwrap_or_default();
        if !matches!(change, "enable" | "disable" | "discard") {
            return Err("change must be one of: enable, disable, discard.".into());
        }
        let not_found = "That rule does not exist for this account. Call list_rules to see the current rules.";
        let rule = lock(&self.store).get_rule(id).map_err(|_| "MiraMail could not read the rules.".to_string())?.filter(|r| r.account == account).ok_or_else(|| not_found.to_string())?;
        match (change, rule.enabled) {
            ("enable", true) => return Err("That rule is already on. Nothing to suggest.".into()),
            ("disable", false) => return Err("That rule is already off. Nothing to suggest.".into()),
            ("enable", false) if rule.spec.is_none() => return Err("That rule is damaged and cannot be turned on; the user has to edit and save it first.".into()),
            _ => {}
        }
        if let Some(spec) = &rule.spec {
            proposals::check_rule_folders(host, &account, spec, change, &|a, f| self.in_scope(a, f))?;
        }
        let mut warnings = Vec::new();
        if let Some(spec) = &rule.spec {
            warnings.push(format!("What the rule does: {}", clean_line(&rules::describe(spec), 300)));
            if change == "enable" {
                warnings.push("only mail that arrives after it is turned on is handled; existing mail is not touched".to_string());
            }
        }
        if change == "discard" {
            warnings.push("the rule is taken out of MiraMail; Undo puts it back (with a new number)".to_string());
        }
        let params = Params {
            account: Some(account.clone()),
            rule_change: Some(proposals::RuleChangeParams { rule_id: rule.id, account, change: change.to_string(), name: clean_line(&rule.name, 100), fingerprint: rules::rule_fingerprint(&rule) }),
            ..Params::default()
        };
        let mut planned = proposals::build_plan(Kind::RuleChange, params, vec![], 0, 0)?;
        planned.warnings.extend(warnings);
        Ok(planned)
    }

    fn plan_create_folder(&mut self, host: &dyn MailActionHost, args: &Value) -> Res<proposals::Planned> {
        let account = self.resolve_account(args["account"].as_str().unwrap_or_default())?;
        let name = proposals::validate_folder_name(args["name"].as_str().unwrap_or_default())?;
        let parent = match args.get("parent").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()) {
            Some(raw) => Some(self.resolve_parent(host, &account, raw)?),
            None => None,
        };
        let full = match parent.as_deref() {
            Some(p) => host.child_folder_name(&account, p, &name)?,
            None => name.clone(),
        };
        if host.find_folder(&account, &full)?.is_some() {
            return Err("A folder with that name already exists.".into());
        }
        let params = Params { account: Some(account), new_folder: Some(name), new_folder_parent: parent, ..Params::default() };
        proposals::build_plan(Kind::CreateFolder, params, vec![], 0, 0)
    }

    /// The parent of a new folder: a folder reference (`f:3`) or the name of an existing folder. It must be
    /// in this conversation's scope (fail closed) and belong to `account`. Returns the server's spelling.
    fn resolve_parent(&self, host: &dyn MailActionHost, account: &str, raw: &str) -> Res<String> {
        if raw.len() <= 16 && raw.starts_with("f:") {
            let s = self.resolve_folder(raw)?;
            if s.account != account {
                return Err("The parent folder belongs to a different account.".into());
            }
            host.check_parent(account, &s.folder)?;
            return Ok(s.folder);
        }
        let existing = host.find_folder(account, raw)?.ok_or_else(|| "The parent folder does not exist. Use an existing folder as the parent.".to_string())?;
        if !self.in_scope(account, &existing) {
            return Err(format!("The folder \"{existing}\" is outside the folders the user allowed for this conversation."));
        }
        host.check_parent(account, &existing)?;
        Ok(existing)
    }

    /// Resolve references (scope + UIDVALIDITY rechecked) and look the messages up; the card is built
    /// from what was found, never from the model's words.
    fn plan_messages(&mut self, host: &dyn MailActionHost, kind: Kind, args: &Value) -> Res<proposals::Planned> {
        let refs: Vec<&str> = args["refs"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if refs.len() > proposals::MAX_ITEMS {
            return Err(format!("A suggestion can hold at most {} messages.", proposals::MAX_ITEMS));
        }
        let mut resolved: Vec<(Scope, MsgMeta)> = Vec::new();
        let mut seen: HashSet<(String, String, u32)> = HashSet::new();
        let mut left_out = 0usize;
        let mut first_err: Option<String> = None;
        for r in refs {
            match self.resolve_message(r) {
                Ok((scope, uid, validity)) => {
                    if !seen.insert((scope.account.clone(), scope.folder.clone(), uid)) {
                        continue;
                    }
                    match host.lookup(&scope.account, &scope.folder, uid) {
                        Ok(Some(m)) if m.uidvalidity == validity && m.account == scope.account && m.folder == scope.folder && m.uid == uid => resolved.push((scope, m)),
                        _ => left_out += 1,
                    }
                }
                Err(e) => {
                    left_out += 1;
                    first_err.get_or_insert(e);
                }
            }
        }
        if resolved.is_empty() {
            return Err(first_err.unwrap_or_else(|| "None of those messages could be found. Search again and use the references from the results.".into()));
        }

        // Destination
        let mut params = Params::default();
        let mut dest: Option<(String, String)> = None;
        match kind {
            Kind::Move => {
                let to_folder = args.get("to_folder").and_then(Value::as_str);
                let to_new = args.get("to_new_folder").and_then(Value::as_str);
                let new_parent = args.get("new_folder_parent").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty());
                if new_parent.is_some() && to_new.is_none() {
                    return Err("new_folder_parent can only be used together with to_new_folder.".into());
                }
                match (to_folder, to_new) {
                    (Some(f), None) => {
                        let s = self.resolve_folder(f)?;
                        dest = Some((s.account, s.folder));
                    }
                    (None, Some(n)) => {
                        let leaf = proposals::validate_folder_name(n)?;
                        let account = resolved[0].0.account.clone();
                        let parent = match new_parent {
                            Some(raw) => Some(self.resolve_parent(host, &account, raw)?),
                            None => None,
                        };
                        let name = match parent.as_deref() {
                            Some(p) => host.child_folder_name(&account, p, &leaf)?,
                            None => leaf,
                        };
                        // Folder names are case-insensitive for our purposes: reuse the server's spelling, and
                        // treat a name that is an existing folder like `to_folder` (it must be in scope).
                        match host.find_folder(&account, &name)? {
                            Some(existing) => {
                                if !self.in_scope(&account, &existing) {
                                    return Err(format!("The folder \"{existing}\" is outside the folders the user allowed for this conversation."));
                                }
                                dest = Some((account, existing));
                            }
                            None => {
                                params.create_dest = true;
                                params.new_folder_parent = parent;
                                dest = Some((account, name));
                            }
                        }
                    }
                    _ => return Err("Give exactly one of to_folder or to_new_folder.".into()),
                }
                let (da, df) = dest.clone().expect("set above");
                params.dest_account = Some(da);
                params.dest_folder = Some(df);
            }
            Kind::Flag => {
                let raw = args.get("flag").and_then(Value::as_str).unwrap_or_default();
                params.flag = Some(FlagOp::parse(raw).ok_or_else(|| "flag must be one of: read, unread, flagged, unflagged.".to_string())?);
            }
            _ => {}
        }

        // Build the rows from the resolved messages.
        let mut roles: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
        let mut items: Vec<NewItem> = Vec::new();
        let mut already_there = 0usize;
        for (scope, m) in resolved {
            let to_folder = match kind {
                Kind::Move => {
                    let (da, df) = dest.clone().expect("move has a destination");
                    if scope.account != da {
                        left_out += 1;
                        continue;
                    }
                    df
                }
                Kind::Archive | Kind::Trash => {
                    let role = if kind == Kind::Trash { RoleFolder::Trash } else { RoleFolder::Archive };
                    let f = match roles.get(&scope.account) {
                        Some(f) => f.clone(),
                        None => {
                            let f = host.role_folder(&scope.account, role)?;
                            roles.insert(scope.account.clone(), f.clone());
                            f
                        }
                    };
                    match f {
                        Some(f) => f,
                        None => {
                            left_out += 1;
                            continue;
                        }
                    }
                }
                _ => String::new(),
            };
            if !to_folder.is_empty() && to_folder == scope.folder {
                already_there += 1;
                continue;
            }
            items.push(NewItem {
                account: scope.account,
                folder: scope.folder,
                uid: m.uid,
                uidvalidity: m.uidvalidity,
                sender: clean_line(&m.from, 120),
                subject: clean_line(&m.subject, 200),
                seen: m.seen,
                flagged: m.flagged,
                to_folder,
            });
        }
        proposals::build_plan(kind, params, items, left_out, already_there)
    }

    async fn t_get_thread(&mut self, args: &Value) -> Res<(String, String)> {
        let r = args["ref"].as_str().unwrap_or_default();
        let (scope, uid, validity) = self.resolve_message(r)?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as u32;
        let items = self.setup.host.get_thread(&self.setup.scopes, &scope, uid, validity, limit).await?;
        let items: Vec<ThreadItem> = items.into_iter().filter(|t| self.in_scope(&t.meta.account, &t.meta.folder)).collect();
        let mut blocks = Vec::new();
        let mut shown = 0;
        let mut truncated = false;
        for it in &items {
            let Some((mref, fref)) = self.mint_message(&it.meta) else { continue };
            let red = self.setup.redaction;
            let excerpt = match &it.body {
                Some(b) => redact_body(&envelope::strip_hidden(b), &red, &mut self.masker).chars().take(THREAD_EXCERPT_CHARS).collect::<String>(),
                None => redact_field(&clean_line(&it.meta.snippet, 240), &red, &mut self.masker),
            };
            let mut text = self.message_header(&it.meta, &fref);
            if !excerpt.is_empty() {
                text.push_str(&format!("\n\n{excerpt}"));
            }
            match self.untrusted(&mref, &text, usize::MAX) {
                Some(b) => {
                    self.note_item(&mref, &it.meta);
                    blocks.push(b);
                    shown += 1;
                }
                None => {
                    truncated = true;
                    break;
                }
            }
        }
        let mut head = format!("{shown} message(s) in the thread of {r}, oldest first. Bodies are excerpts; use get_message for the full text.");
        if truncated {
            head.push_str("\nOutput cut short: the per-question limit for mail content was reached.");
        }
        Ok((format!("{head}\n{}", blocks.join("\n")), plural(shown, "message")))
    }
}

fn fail(label: String, message: String, summary: &str) -> ToolOutcome {
    ToolOutcome { content: message, ok: false, label, summary: summary.into() }
}

fn budget_msg() -> String {
    "The limit for mail content in one question was reached. Answer with what you have already read.".into()
}

fn kind_name(k: char) -> &'static str {
    match k {
        'a' => "account",
        'f' => "folder",
        'c' => "calendar",
        'e' => "event",
        _ => "message",
    }
}

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn plural_results(shown: usize, total: u32) -> String {
    if total as usize > shown {
        format!("{shown} of {total} results")
    } else {
        plural(shown, "result")
    }
}

/// `YYYY-MM-DD HH:MM UTC`.
pub fn fmt_ts(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", secs / 3600, (secs % 3600) / 60)
}

/// UI chip text from the call's arguments (model-controlled, so cleaned and clipped; shown as text only).
pub fn label_for(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(|v| clean_line(v, 80)).unwrap_or_default();
    match name {
        "list_accounts" | "list_folders" | "mail_stats" | "list_rules" => String::new(),
        "search_mail" => s("query"),
        "server_search" => {
            let mut parts = Vec::new();
            for k in ["from", "to", "subject", "text", "since", "before"] {
                let v = s(k);
                if !v.is_empty() {
                    parts.push(format!("{k}:{v}"));
                }
            }
            if parts.is_empty() {
                s("folder")
            } else {
                parts.join(" ")
            }
        }
        "get_message" | "get_thread" => s("ref"),
        "propose_move" | "propose_archive" | "propose_trash" | "propose_flag" => {
            let n = args.get("refs").and_then(Value::as_array).map_or(0, Vec::len);
            format!("{n} message{}", if n == 1 { "" } else { "s" })
        }
        "propose_create_folder" | "propose_rule" => s("name"),
        "propose_rule_change" => format!("{} rule {}", s("change"), args.get("rule").and_then(Value::as_i64).unwrap_or(0)),
        "draft_reply" => s("ref"),
        "draft_new" => s("subject"),
        "list_calendars" => String::new(),
        "list_events" | "free_busy" => format!("{} \u{2192} {}", s("from"), s("to")),
        "find_free_slots" => format!("{} min, {} \u{2192} {}", args.get("duration_minutes").and_then(Value::as_u64).unwrap_or(0), s("from"), s("to")),
        "get_event" | "propose_event_change" | "propose_event_cancel" | "propose_invite_response" => s("ref"),
        "propose_event" => s("title"),
        _ => clean_line(name, 40),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_timestamps() {
        assert_eq!(fmt_ts(0), "1970-01-01 00:00 UTC");
        assert_eq!(fmt_ts(1_793_000_000), "2026-10-26 07:33 UTC");
        assert_eq!(fmt_ts(-86_400), "1969-12-31 00:00 UTC");
    }

    #[test]
    fn labels_are_short_clean_and_argument_derived() {
        assert_eq!(label_for("search_mail", &json!({"query":"from:foo"})), "from:foo");
        assert_eq!(label_for("search_mail", &json!({"query":"a\u{202e}b\nc"})), "ab c");
        assert_eq!(label_for("search_mail", &json!({"query":"x".repeat(500)})).chars().count(), 81);
        assert_eq!(label_for("server_search", &json!({"folder":"f:1","from":"bob","since":"2026-01-01"})), "from:bob since:2026-01-01");
        assert_eq!(label_for("server_search", &json!({"folder":"f:1"})), "f:1");
        assert_eq!(label_for("get_message", &json!({"ref":"m:4"})), "m:4");
        assert_eq!(label_for("list_accounts", &json!({})), "");
        assert_eq!(label_for("search_mail", &Value::Null), "");
        assert_eq!(label_for("weird\nname", &json!({})), "weird name");
    }

    struct NoHost;
    impl MailToolHost for NoHost {
        fn accounts(&self) -> BoxFut<'_, Result<Vec<String>, String>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn folders(&self, _a: &str) -> BoxFut<'_, Result<Vec<FolderMeta>, String>> {
            Box::pin(async { Err("x".into()) })
        }
        fn folder_validity(&self, _a: &str, _f: &str) -> Result<Option<u32>, String> {
            Ok(None)
        }
        fn search(&self, _s: &[Scope], _q: &str, _l: u32, _o: u32) -> BoxFut<'_, Result<SearchOut, String>> {
            Box::pin(async { Err("x".into()) })
        }
        fn server_search(&self, _s: &Scope, _v: Option<u32>, _q: &ServerSearchArgs, _l: u32) -> BoxFut<'_, Result<Vec<MsgMeta>, String>> {
            Box::pin(async { Err("x".into()) })
        }
        fn stats(&self, _s: &[Scope]) -> BoxFut<'_, Result<StatsOut, String>> {
            Box::pin(async { Err("x".into()) })
        }
        fn get_message(&self, _s: &Scope, _u: u32, _v: u32) -> BoxFut<'_, Result<MsgText, String>> {
            Box::pin(async { Err("x".into()) })
        }
        fn get_thread(&self, _s: &[Scope], _c: &Scope, _u: u32, _v: u32, _l: u32) -> BoxFut<'_, Result<Vec<ThreadItem>, String>> {
            Box::pin(async { Err("x".into()) })
        }
    }

    fn rules_rt(accounts: &[&str]) -> (ToolRuntime, Arc<Mutex<Store>>) {
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let setup = ToolSetup {
            host: Arc::new(NoHost),
            scopes: accounts.iter().map(|a| Scope { account: (*a).into(), folder: "INBOX".into() }).collect(),
            redaction: RedactOpts::default(),
            actions: None,
            drafts: None,
            calendar: None,
        };
        (ToolRuntime::new(setup, store.clone(), 1), store)
    }

    fn add_rule(store: &Arc<Mutex<Store>>, account: &str, name: &str, origin: &'static str, enabled: bool) {
        use crate::rules::{Action, Condition, Field, MatchMode, NewRule, Op, RuleSpec, SPEC_VERSION};
        let spec = RuleSpec { v: SPEC_VERSION, mode: MatchMode::All, conditions: vec![Condition { field: Field::Subject, op: Op::Contains, value: "invoice".into() }], actions: vec![Action::Archive] };
        lock(store).create_rule(&NewRule { account: account.into(), name: name.into(), spec, origin, conv_id: None, reason: "SECRET-REASON".into(), enabled }, 1).unwrap();
    }

    #[test]
    fn list_rules_shape_scope_and_off_state() {
        let (mut rt, store) = rules_rt(&["a@x.org"]);
        add_rule(&store, "a@x.org", "Invoices", "user", true);
        add_rule(&store, "a@x.org", "Drafty", "ai", false);
        add_rule(&store, "other@y.org", "Hidden rule", "user", true);
        let (out, summary) = rt.t_list_rules(&json!({})).unwrap();
        assert_eq!(summary, "2 rules");
        assert!(out.contains("Invoices") && out.contains("on, made by user") && out.contains("archive it"), "{out}");
        assert!(out.contains("Drafty") && out.contains("off, made by ai"), "{out}");
        assert!(out.contains("(account a:1,") && out.contains("<<<UNTRUSTED"), "{out}");
        assert!(!out.contains("Hidden rule") && !out.contains("other@y.org") && !out.contains("a@x.org"), "{out}");
        assert!(!out.contains("SECRET-REASON"));
        // an out-of-scope account argument is refused
        assert!(rt.t_list_rules(&json!({"account":"a:9"})).is_err());
    }

    #[test]
    fn list_rules_empty_and_capped() {
        let (mut rt, store) = rules_rt(&["a@x.org"]);
        assert_eq!(rt.t_list_rules(&json!({})).unwrap().1, "0 rules");
        for i in 0..(MAX_RULES_LISTED + 5) {
            add_rule(&store, "a@x.org", &format!("Rule number {i}"), "user", true);
        }
        let (out, summary) = rt.t_list_rules(&json!({})).unwrap();
        assert_eq!(summary, format!("{MAX_RULES_LISTED} of {} rules", MAX_RULES_LISTED + 5));
        assert!(out.contains("5 more not shown"));
        assert_eq!(out.matches("\nrule ").count(), MAX_RULES_LISTED);
        // the untrusted budget applies
        let (mut rt2, _s) = rules_rt(&["a@x.org"]);
        add_rule(&_s, "a@x.org", "Invoices", "user", true);
        rt2.set_char_budget(10);
        assert!(rt2.t_list_rules(&json!({})).is_err());
    }
}
