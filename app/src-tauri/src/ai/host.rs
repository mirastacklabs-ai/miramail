//! `MailToolHost` over mailcore: the read-only mail access the AI tools use (AI chat P1).
//!
//! Everything here is READ-only: local FTS5 search, cached messages, and two network paths that
//! never change mail (IMAP `UID SEARCH` + header fetch, and `BODY.PEEK[]` for a body that is not
//! cached yet). Both go to the user's own mail server through the same per-account connection lock
//! the rest of the app uses. Scope and UIDVALIDITY checks are done by the `aichat` engine before
//! these methods run; the scope list is still applied here (defence in depth).
//!
//! Never logs; errors returned to the engine are generic strings (no mail content).
//!
//! The second half of the file implements [`MailActionHost`] (AI chat P2). It is used ONLY by the
//! proposal executor after the user pressed Approve / Undo; the model never reaches it. All changes
//! go through the set-safe batch operations in `mailcore::batch` and then update the local cache.

use std::sync::Arc;
use std::time::Duration;

use aichat::drafts::{parse_addr_list, valid_address, Addr, DraftHost, ReplySource};
use aichat::envelope::clean_line;
use aichat::proposals::{FlagReq, FlagRes, MailActionHost, MoveItemRes, MoveReq, MoveRes, RoleFolder};
use aichat::tools::{
    BoxFut, FolderMeta, FolderStat, MailToolHost, MsgMeta, MsgText, Scope, SearchOut, ServerSearchArgs, StatsOut, ThreadItem,
};
use mailcore::imap::find_role;
use mailcore::{BatchItem, FlagChange, FolderRole, ScopeKey, SearchParams, ServerQuery, Storage};

use crate::state::Conn;

/// How the host gets an authenticated IMAP connection (production: `AppState::connect`).
pub type ConnectFn = Arc<dyn Fn(String) -> BoxFut<'static, Result<Conn, String>> + Send + Sync>;

const NET_TIMEOUT: Duration = Duration::from_secs(60);
const BACKFILL_PER_CALL: u32 = 100;

pub struct AppMailHost {
    storage: Arc<Storage>,
    connect: Option<ConnectFn>,
}

impl AppMailHost {
    pub fn new(storage: Arc<Storage>, connect: Option<ConnectFn>) -> Self {
        AppMailHost { storage, connect }
    }
}

fn db_err<E>(_: E) -> String {
    "The local mail cache could not be read.".to_string()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn local_offset_secs() -> i64 {
    i64::from(chrono::Local::now().offset().local_minus_utc())
}

fn meta_of(m: &mailcore::MessageSummary, bulk: bool) -> MsgMeta {
    MsgMeta {
        account: m.account.clone(),
        folder: m.folder.clone(),
        uid: m.uid,
        uidvalidity: m.uidvalidity,
        subject: m.subject.clone(),
        from: m.from.clone(),
        to: m.to.clone(),
        date: m.date,
        seen: m.flags.seen,
        flagged: m.flags.flagged,
        answered: m.flags.answered,
        snippet: m.snippet.clone(),
        bulk,
    }
}

fn keys(scopes: &[Scope]) -> Vec<ScopeKey> {
    scopes.iter().map(|s| ScopeKey::new(s.account.clone(), s.folder.clone())).collect()
}

/// Map a mailcore error to text the model may see. Our own validation messages pass through;
/// server / network text does not.
fn net_err(e: mailcore::Error) -> String {
    match e {
        mailcore::Error::Invalid(m) => m,
        mailcore::Error::AuthFailed => "The mail server rejected the saved login.".into(),
        mailcore::Error::Timeout(_) => "The mail server did not answer in time.".into(),
        mailcore::Error::Imap(m) if m.contains("UIDVALIDITY") => "The folder changed on the server; search again.".into(),
        _ => "The mail server could not complete the request.".into(),
    }
}

impl AppMailHost {
    /// `meta_of` with a snippet the model may see: rebuilt from the cached body with hidden HTML content removed.
    /// The stored snippet comes from the raw text (hidden preheaders included), so it is never passed on; with no
    /// cached body there is no snippet.
    fn ai_meta(&self, m: &mailcore::MessageSummary, bulk: bool) -> Result<MsgMeta, String> {
        let mut meta = meta_of(m, bulk);
        meta.snippet = self
            .storage
            .get_ai_text(&m.account, &m.folder, m.uid)
            .map_err(db_err)?
            .map(|t| mailcore::mime::make_snippet(&t))
            .unwrap_or_default();
        Ok(meta)
    }
}

impl MailToolHost for AppMailHost {
    fn accounts(&self) -> BoxFut<'_, Result<Vec<String>, String>> {
        Box::pin(async move { Ok(self.storage.list_accounts().map_err(db_err)?.into_iter().map(|a| a.email).collect()) })
    }

    fn folders(&self, account: &str) -> BoxFut<'_, Result<Vec<FolderMeta>, String>> {
        let account = account.to_string();
        Box::pin(async move {
            let mut out = Vec::new();
            for f in self.storage.list_folders(&account).map_err(db_err)?.into_iter().filter(|f| f.selectable) {
                let st = self.storage.folder_stats(&account, &f.name).map_err(db_err)?;
                out.push(FolderMeta { name: f.name, role: f.role.as_str().to_string(), total: st.total, unread: st.unread });
            }
            Ok(out)
        })
    }

    fn folder_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        Ok(self.storage.folder_sync_state(account, folder).map_err(db_err)?.0)
    }

    fn search(&self, scopes: &[Scope], query: &str, limit: u32, offset: u32) -> BoxFut<'_, Result<SearchOut, String>> {
        let (scopes, query) = (keys(scopes), query.to_string());
        Box::pin(async move {
            // Lazy backfill of body text for messages cached before the index existed.
            let _ = self.storage.backfill_body_text(BACKFILL_PER_CALL);
            let r = self
                .storage
                .search_messages(&SearchParams { query, scopes, limit, offset, now: now_secs(), tz_offset: local_offset_secs() })
                .map_err(db_err)?;
            Ok(SearchOut { total: r.total, hints: r.hints, hits: r.hits.iter().map(|h| self.ai_meta(&h.summary, h.bulk)).collect::<Result<_, _>>()? })
        })
    }

    fn server_search(&self, scope: &Scope, expected_validity: Option<u32>, q: &ServerSearchArgs, limit: u32) -> BoxFut<'_, Result<Vec<MsgMeta>, String>> {
        let scope = scope.clone();
        let q = ServerQuery {
            from: q.from.clone(),
            to: q.to.clone(),
            subject: q.subject.clone(),
            text: q.text.clone(),
            since: q.since.clone(),
            before: q.before.clone(),
            unseen: q.unseen,
            flagged: None,
        };
        Box::pin(async move {
            let connect = self.connect.clone().ok_or("Searching the mail server is not available right now.")?;
            // Reject bad criteria before opening a connection.
            mailcore::build_search_criteria(&q).map_err(net_err)?;
            let mut conn = connect(scope.account.clone()).await.map_err(|_| "Could not connect to the mail server.".to_string())?;
            let res = tokio::time::timeout(NET_TIMEOUT, conn.client.server_search(&self.storage, &scope.account, &scope.folder, expected_validity, &q, limit)).await;
            conn.close().await;
            let (_validity, uids) = res.map_err(|_| "The mail server did not answer in time.".to_string())?.map_err(net_err)?;
            let mut out = Vec::new();
            for uid in uids {
                if let Some(m) = self.storage.get_message(&scope.account, &scope.folder, uid).map_err(db_err)? {
                    out.push(self.ai_meta(&m, false)?);
                }
            }
            Ok(out)
        })
    }

    fn stats(&self, scopes: &[Scope]) -> BoxFut<'_, Result<StatsOut, String>> {
        let scopes = scopes.to_vec();
        Box::pin(async move {
            let mut folders = Vec::new();
            for s in &scopes {
                let st = self.storage.folder_stats(&s.account, &s.folder).map_err(db_err)?;
                folders.push(FolderStat { account: s.account.clone(), folder: s.folder.clone(), total: st.total, unread: st.unread, flagged: st.flagged, oldest: st.oldest, newest: st.newest });
            }
            let top_senders = self.storage.top_senders(&keys(&scopes), 10).map_err(db_err)?;
            Ok(StatsOut { folders, top_senders })
        })
    }

    fn get_message(&self, scope: &Scope, uid: u32, uidvalidity: u32) -> BoxFut<'_, Result<MsgText, String>> {
        let scope = scope.clone();
        Box::pin(async move {
            let m = self
                .storage
                .get_message(&scope.account, &scope.folder, uid)
                .map_err(db_err)?
                .filter(|m| m.uidvalidity == uidvalidity)
                .ok_or("That message is no longer in the local copy. Search again.")?;
            let raw = self.raw_body(&scope, uid).await?;
            let parsed = mailcore::parse_message(&raw).ok();
            // Text the user can read: hidden HTML content is removed and HTML wins over a differing plain part.
            let body = match parsed.as_ref() {
                Some(p) => p.ai_text(),
                None => self.storage.get_body_text(&scope.account, &scope.folder, uid).map_err(db_err)?.unwrap_or_default(),
            };
            let extra = self.storage.get_extra_headers(&scope.account, &scope.folder, uid).map_err(db_err)?.unwrap_or_default();
            let attachments = parsed.map(|p| p.attachments.into_iter().filter(|a| !a.inline).map(|a| a.name).collect()).unwrap_or_default();
            let bulk = !extra.list_id.is_empty();
            let mut meta = meta_of(&m, bulk);
            meta.snippet = mailcore::mime::make_snippet(&body);
            Ok(MsgText { meta, body, list_id: extra.list_id, reply_to: extra.reply_to, attachments })
        })
    }

    fn get_thread(&self, scopes: &[Scope], scope: &Scope, uid: u32, uidvalidity: u32, limit: u32) -> BoxFut<'_, Result<Vec<ThreadItem>, String>> {
        let (scopes, scope) = (keys(scopes), scope.clone());
        Box::pin(async move {
            let msgs = self.storage.thread_of(&scope.account, &scope.folder, uid, uidvalidity, &scopes, limit).map_err(db_err)?;
            let mut out = Vec::new();
            for m in &msgs {
                let body = match self.storage.get_ai_text(&m.account, &m.folder, m.uid).map_err(db_err)? {
                    Some(t) => Some(t),
                    None => self.storage.get_body_text(&m.account, &m.folder, m.uid).map_err(db_err)?,
                };
                out.push(ThreadItem { meta: self.ai_meta(m, false)?, body });
            }
            Ok(out)
        })
    }
}

// ---------------------------------------------------------------------------------------------
// MailActionHost: executes approved proposals
// ---------------------------------------------------------------------------------------------

/// Whole-batch ceiling (individual commands have their own timeouts inside mailcore).
const ACTION_TIMEOUT: Duration = Duration::from_secs(600);

impl AppMailHost {
    /// The raw message: cached, else fetched with BODY.PEEK[] (does not mark it read) and cached.
    async fn raw_body(&self, scope: &Scope, uid: u32) -> Result<Vec<u8>, String> {
        if let Some(raw) = self.storage.get_body(&scope.account, &scope.folder, uid).map_err(db_err)? {
            return Ok(raw);
        }
        let connect = self.connect.clone().ok_or("The message body is not cached and the mail server is not available right now.")?;
        let mut conn = connect(scope.account.clone()).await.map_err(|_| "Could not connect to the mail server.".to_string())?;
        let res = tokio::time::timeout(NET_TIMEOUT, conn.client.get_body(&self.storage, &scope.account, &scope.folder, uid)).await;
        conn.close().await;
        res.map_err(|_| "The mail server did not answer in time.".to_string())?.map_err(net_err)
    }
}

/// Drafts (AI chat P3): READ-only. The recipients of a reply are computed from this data by `aichat::drafts`;
/// nothing here (or anywhere in this module) can send mail.
impl DraftHost for AppMailHost {
    fn reply_source(&self, scope: &Scope, uid: u32, uidvalidity: u32) -> BoxFut<'_, Result<ReplySource, String>> {
        let scope = scope.clone();
        Box::pin(async move {
            let m = self
                .storage
                .get_message(&scope.account, &scope.folder, uid)
                .map_err(db_err)?
                .filter(|m| m.uidvalidity == uidvalidity)
                .ok_or("That message is no longer in the local copy. Search again.")?;
            let raw = self.raw_body(&scope, uid).await?;
            let parsed = mailcore::parse_message(&raw).map_err(|_| "That message could not be read.".to_string())?;
            let extra = self.storage.get_extra_headers(&scope.account, &scope.folder, uid).map_err(db_err)?.unwrap_or_default();
            // Only plain, valid addresses become recipients; display names are cleaned (no hidden characters).
            let conv = |l: &[mailcore::EmailAddress]| -> Vec<Addr> {
                l.iter().filter(|a| valid_address(&a.address)).map(|a| Addr { name: clean_line(a.name.as_deref().unwrap_or_default(), 60), address: a.address.clone() }).collect()
            };
            let in_sent = self.storage.list_folders(&scope.account).map_err(db_err)?.iter().any(|f| f.name == scope.folder && f.role == FolderRole::Sent);
            let body = parsed.ai_text();
            Ok(ReplySource {
                subject: m.subject.clone(),
                from: conv(&parsed.headers.from),
                reply_to: parse_addr_list(&extra.reply_to),
                to: conv(&parsed.headers.to),
                cc: conv(&parsed.headers.cc),
                message_id: parsed.headers.message_id.clone().or(m.message_id.clone()),
                references: parsed.headers.references.clone(),
                date: parsed.headers.date.unwrap_or(m.date),
                body,
                in_sent,
            })
        })
    }

    fn address_known(&self, address: &str) -> Result<bool, String> {
        self.storage.address_seen(address).map_err(db_err)
    }

    fn own_addresses(&self, account: &str) -> Result<Vec<String>, String> {
        // The account id is the user's address.
        Ok(vec![account.to_string()])
    }
}

/// After this error, may the server have executed the command? `false` for errors raised before any
/// command is sent (invalid request, UIDVALIDITY mismatch, login).
fn command_may_have_run(e: &mailcore::Error) -> bool {
    match e {
        mailcore::Error::Invalid(_) | mailcore::Error::AuthFailed => false,
        mailcore::Error::Imap(m) if m.contains("UIDVALIDITY") => false,
        _ => true,
    }
}

impl AppMailHost {
    /// A move failed or timed out after the command may have been sent: reconnect, look at the source
    /// and destination (UID presence, then exact Message-ID) and report each item as moved, not moved,
    /// or unknown. If the check itself fails everything is `unknown`. Keeps the cache consistent for
    /// items that are confirmed moved.
    async fn settle_after_failure(&self, req: &MoveReq, items: &[BatchItem], why: &str) -> MoveRes {
        use mailcore::MoveVerdict;
        let unknown = |why: &str| MoveRes {
            dest_validity: None,
            items: items.iter().map(|it| MoveItemRes { uid: it.uid, ok: false, error: Some(why.to_string()), new_uid: None, message_id: it.message_id.clone(), unknown: true }).collect(),
        };
        let Ok(mut conn) = self.connect_for(&req.account).await else { return unknown(why) };
        let rep = tokio::time::timeout(Duration::from_secs(120), conn.client.verify_move(&req.from_folder, req.from_validity, &req.to_folder, items)).await;
        conn.close().await;
        let Ok(Ok(rep)) = rep else { return unknown(why) };
        let out = items
            .iter()
            .zip(rep.verdicts)
            .map(|(it, v)| match v {
                MoveVerdict::NotMoved => MoveItemRes { uid: it.uid, ok: false, error: Some(why.to_string()), new_uid: None, message_id: it.message_id.clone(), unknown: false },
                MoveVerdict::Moved(new_uid) => {
                    if let Some(u) = it.uid {
                        let _ = self.storage.delete_message(&req.account, &req.from_folder, u);
                    }
                    MoveItemRes { uid: it.uid, ok: true, error: None, new_uid, message_id: it.message_id.clone(), unknown: false }
                }
                MoveVerdict::Unknown => MoveItemRes { uid: it.uid, ok: false, error: Some(why.to_string()), new_uid: None, message_id: it.message_id.clone(), unknown: true },
            })
            .collect();
        MoveRes { dest_validity: rep.dest_validity, items: out }
    }

    async fn connect_for(&self, account: &str) -> Result<Conn, String> {
        let connect = self.connect.clone().ok_or("The mail server is not available right now.")?;
        connect(account.to_string()).await.map_err(|_| "Could not connect to the mail server.".to_string())
    }
}

impl MailActionHost for AppMailHost {
    fn folder_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        Ok(self.storage.folder_sync_state(account, folder).map_err(db_err)?.0)
    }

    fn lookup(&self, account: &str, folder: &str, uid: u32) -> Result<Option<MsgMeta>, String> {
        match self.storage.get_message(account, folder, uid).map_err(db_err)? {
            Some(m) => Ok(Some(self.ai_meta(&m, false)?)),
            None => Ok(None),
        }
    }

    fn role_folder(&self, account: &str, role: RoleFolder) -> Result<Option<String>, String> {
        let folders = self.storage.list_folders(account).map_err(db_err)?;
        let want = match role {
            RoleFolder::Trash => FolderRole::Trash,
            RoleFolder::Archive => FolderRole::Archive,
        };
        Ok(find_role(&folders, want).map(|f| f.name.clone()))
    }

    fn folder_exists(&self, account: &str, folder: &str) -> Result<bool, String> {
        Ok(self.storage.list_folders(account).map_err(db_err)?.iter().any(|f| f.name == folder))
    }

    fn find_folder(&self, account: &str, name: &str) -> Result<Option<String>, String> {
        Ok(self.storage.list_folders(account).map_err(db_err)?.into_iter().find(|f| f.name.eq_ignore_ascii_case(name)).map(|f| f.name))
    }

    fn move_batch(&self, req: MoveReq) -> BoxFut<'_, Result<MoveRes, String>> {
        Box::pin(async move {
            // Message-IDs let an undo find a message whose new UID is unknown.
            let items: Vec<BatchItem> = req
                .items
                .iter()
                .map(|i| BatchItem {
                    uid: i.uid,
                    message_id: i.message_id.clone().or_else(|| {
                        i.uid.and_then(|u| self.storage.get_message(&req.account, &req.from_folder, u).ok().flatten()).and_then(|m| m.message_id)
                    }),
                })
                .collect();
            let mut conn = self.connect_for(&req.account).await?;
            let res = tokio::time::timeout(ACTION_TIMEOUT, conn.client.batch_move(&req.from_folder, req.from_validity, &req.to_folder, &items)).await;
            let report = match res {
                Ok(Ok(r)) => r,
                // Refused before any command could have run (bad request, UIDVALIDITY changed, login).
                Ok(Err(e)) if !command_may_have_run(&e) => {
                    conn.close().await;
                    return Err(net_err(e));
                }
                // Timeout or connection error: the server may have executed the MOVE and the reply was
                // lost. Do not call that a failure; look at the server on a fresh connection.
                Ok(Err(e)) => {
                    conn.close().await;
                    return Ok(self.settle_after_failure(&req, &items, &net_err(e)).await);
                }
                Err(_) => {
                    conn.close().await;
                    return Ok(self.settle_after_failure(&req, &items, "The mail server did not answer in time.").await);
                }
            };
            // Keep the local cache consistent: the moved messages are gone from the source folder.
            for o in report.outcomes.iter().filter(|o| o.ok) {
                if let Some(u) = o.uid {
                    let _ = self.storage.delete_message(&req.account, &req.from_folder, u);
                }
            }
            // Bring the destination up to date (best effort), so later steps (flags after a rule's move, Undo) find
            // the moved messages at their new place in the local copy.
            if report.outcomes.iter().any(|o| o.ok) {
                let _ = tokio::time::timeout(Duration::from_secs(60), conn.client.sync_folder(&self.storage, &req.account, &req.to_folder, mailcore::imap::DEFAULT_SYNC_LIMIT)).await;
            }
            conn.close().await;
            Ok(MoveRes {
                dest_validity: report.dest_validity,
                items: report
                    .outcomes
                    .into_iter()
                    .zip(items)
                    .map(|(o, it)| MoveItemRes { uid: o.uid, ok: o.ok, error: o.error, new_uid: o.new_uid, message_id: it.message_id, unknown: false })
                    .collect(),
            })
        })
    }

    fn flag_batch(&self, req: FlagReq) -> BoxFut<'_, Result<FlagRes, String>> {
        Box::pin(async move {
            let change = match req.flag.as_str() {
                "seen" => FlagChange::Seen(req.value),
                "flagged" => FlagChange::Flagged(req.value),
                _ => return Err("Unsupported flag.".into()),
            };
            let mut conn = self.connect_for(&req.account).await?;
            let res = tokio::time::timeout(ACTION_TIMEOUT, conn.client.batch_flag(&req.folder, req.validity, &req.uids, change)).await;
            conn.close().await;
            let report = res.map_err(|_| "The mail server did not answer in time.".to_string())?.map_err(net_err)?;
            for (uid, ok, _) in &report.outcomes {
                if !*ok {
                    continue;
                }
                if let Ok(Some(m)) = self.storage.get_message(&req.account, &req.folder, *uid) {
                    let mut f = m.flags;
                    match change {
                        FlagChange::Seen(v) => f.seen = v,
                        FlagChange::Flagged(v) => f.flagged = v,
                    }
                    let _ = self.storage.update_flags(&req.account, &req.folder, *uid, f);
                }
            }
            Ok(FlagRes { items: report.outcomes, old: report.old })
        })
    }

    fn child_folder_name(&self, account: &str, parent: &str, leaf: &str) -> Result<String, String> {
        let folders = self.storage.list_folders(account).map_err(db_err)?;
        mailcore::foldermgmt::child_full_name(&folders, Some(parent), leaf)
    }

    fn account_folders(&self, account: &str) -> Result<Vec<String>, String> {
        Ok(self.storage.list_folders(account).map_err(db_err)?.into_iter().map(|f| f.name).collect())
    }

    fn check_parent(&self, account: &str, parent: &str) -> Result<(), String> {
        let folders = self.storage.list_folders(account).map_err(db_err)?;
        mailcore::foldermgmt::parent_allows_children(&folders, parent)
    }

    fn create_folder_named(&self, account: &str, full_name: &str) -> BoxFut<'_, Result<bool, String>> {
        let (account, full) = (account.to_string(), full_name.to_string());
        Box::pin(async move {
            let mut conn = self.connect_for(&account).await?;
            let r = tokio::time::timeout(Duration::from_secs(60), async {
                let existing = conn.client.list_folders().await?;
                if existing.iter().any(|f| f.name.eq_ignore_ascii_case(&full)) {
                    return Ok::<bool, mailcore::Error>(false);
                }
                conn.client.create_folder_named(&full).await?;
                Ok(true)
            })
            .await;
            let created = match r {
                Err(_) => {
                    conn.close().await;
                    return Err("The mail server did not answer in time.".into());
                }
                Ok(Err(e)) => {
                    conn.close().await;
                    return Err(net_err(e));
                }
                Ok(Ok(c)) => c,
            };
            if let Ok(Ok(list)) = tokio::time::timeout(Duration::from_secs(30), conn.client.list_folders()).await {
                let _ = self.storage.replace_folders(&account, &list);
            }
            conn.close().await;
            Ok(created)
        })
    }

    fn create_folder(&self, account: &str, name: &str) -> BoxFut<'_, Result<bool, String>> {
        let (account, name) = (account.to_string(), name.to_string());
        Box::pin(async move {
            let mut conn = self.connect_for(&account).await?;
            let created = tokio::time::timeout(Duration::from_secs(60), conn.client.create_folder(&name)).await;
            let created = match created {
                Err(_) => {
                    conn.close().await;
                    return Err("The mail server did not answer in time.".into());
                }
                Ok(Err(e)) => {
                    conn.close().await;
                    return Err(net_err(e));
                }
                Ok(Ok(c)) => c,
            };
            // Refresh the cached folder list so the new folder shows up.
            if let Ok(Ok(list)) = tokio::time::timeout(Duration::from_secs(30), conn.client.list_folders()).await {
                let _ = self.storage.replace_folders(&account, &list);
            }
            conn.close().await;
            Ok(created)
        })
    }
}
