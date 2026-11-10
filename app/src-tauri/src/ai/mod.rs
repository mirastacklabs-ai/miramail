//! AI chat glue (task A3a): `ai.db`, Keychain-held API keys, profiles, settings, conversations and
//! the streaming turn. All chat logic lives in the `aichat` crate; this module only adds the app
//! policy (key binding, validation, purge) and the Tauri-independent operations the commands call.
//!
//! Rule 4 (CLAUDE.md): nothing here touches the network at startup. The AI endpoint is contacted
//! only by [`AiState::test_profile`] (no mail data) and [`AiState::send`].

pub mod calhost;
pub mod commands;
pub mod host;
pub mod tone;
pub mod wire;

use aichat::budget::Caps;
use aichat::engine::{self, system_clock, EventSink, InFlight, ProfileRuntime, TurnCtx, DEFAULT_TITLE};
use aichat::drafts::{self, DraftHost};
use aichat::cal::{CalActionHost, CalendarHost};
use aichat::calexec::{self, CalExecutor};
use aichat::proposals::{self, Executor, MailActionHost};
use aichat::http::{self, Auth, BaseUrl, ErrorKind, Limits, UrlWarning, UserFacingError};
use aichat::provider::{self, MaxTokensField, ProviderOptions, StreamParser, DEFAULT_ANTHROPIC_VERSION};
use aichat::sse::SseParser;
use aichat::redact::RedactOpts;
use aichat::store::{self, NewConversation, ProfileInput, PurgeSettings, Store, StoredMessage, ToolRun};
use aichat::tools::{CalSetup, MailToolHost, Scope as ToolScope, ToolSetup};
use aichat::types::Part;
use aichat::types::{ChatMessage, ChatRequest, Dialect, StreamEvent, ToolSpec};
use futures_util::StreamExt;
use mailcore::SecretStore;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wire::*;

pub const KEY_PREFIX: &str = "ai-provider:";
pub const DEFAULT_MAX_OUTPUT: u32 = 4096;
const MAX_OUTPUT_LIMIT: u32 = 128_000;
const KEYCHAIN_ERR: &str = "Couldn't reach the macOS Keychain to read or save the API key.";
const DB_ERR: &str = "MiraMail couldn't read or write its AI chat database.";
const KEY_REENTER: &str = "The address of this AI endpoint changed since the API key was saved, so the key can't be used. Enter the key again (or clear it) in Settings > AI.";

/// Keychain account of a profile's API key. Mail accounts always contain '@'; profile ids never do.
pub fn key_account(profile_id: &str) -> String {
    format!("{KEY_PREFIX}{profile_id}")
}

pub struct AiState {
    store: Arc<Mutex<Store>>,
    secrets: Arc<dyn SecretStore>,
    inflight: InFlight,
    /// Overridable so tests run fast; production uses the defaults.
    limits: Limits,
    /// Read-only mail access for the AI tools, installed once the app state exists (`None` = plain chat).
    mail: Mutex<Option<Arc<dyn MailToolHost>>>,
    /// Write access, used only by the proposal executor after an explicit approval / undo.
    actions: Mutex<Option<Arc<dyn MailActionHost>>>,
    /// Read-only access for drafts (P3). Offered to the model only with tools + suggest consent.
    drafts: Mutex<Option<Arc<dyn DraftHost>>>,
    busy: proposals::Busy,
    /// Serialises "claim a proposal + read the consent" (approve) against "check busy + write the consent"
    /// (Edit permissions): whichever comes first wins and the other sees it.
    consent_gate: tokio::sync::Mutex<()>,
    consent_check_timeout: std::time::Duration,
    #[cfg(test)]
    after_claim_hook: Mutex<Option<TestHook>>,
    /// Read-only calendar access for the calendar tools (`None` = no calendar tools).
    cal: Mutex<Option<Arc<dyn CalendarHost>>>,
    /// Calendar write access, used only by the calendar executor after an explicit approval / undo.
    cal_actions: Mutex<Option<Arc<dyn CalActionHost>>>,
    /// Turns a rule on the way the Rules screen does (cursor baseline); used only when the user approves a rule change.
    rule_hook: Mutex<Option<Arc<dyn proposals::RuleHook>>>,
    /// Test seam for the per-turn environment note (production reads the clock and the Mac's zone).
    env_source: Mutex<Option<Arc<dyn Fn() -> aichat::context::EnvInfo + Send + Sync>>>,
}

type R<T> = Result<T, String>;

/// Test seam: runs after approve has claimed the proposal and read the consent, before anything executes.
#[cfg(test)]
pub(crate) type TestHook = Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

impl AiState {
    pub fn new(store: Store, secrets: Arc<dyn SecretStore>) -> Self {
        AiState { store: Arc::new(Mutex::new(store)), secrets, inflight: InFlight::new(), limits: Limits::default(), mail: Mutex::new(None), actions: Mutex::new(None), drafts: Mutex::new(None), busy: Arc::new(Mutex::new(Default::default())), consent_gate: tokio::sync::Mutex::new(()), consent_check_timeout: std::time::Duration::from_secs(10), #[cfg(test)] after_claim_hook: Mutex::new(None), cal: Mutex::new(None), cal_actions: Mutex::new(None), rule_hook: Mutex::new(None), env_source: Mutex::new(None) }
    }

    /// Open `ai.db` (created on first use). No network.
    pub fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> R<Self> {
        let s = Store::open(path).map_err(|e| format!("couldn't open the AI chat database: {e}"))?;
        Ok(Self::new(s, secrets))
    }

    /// Install the mail access used by conversations that enabled tools.
    pub fn set_mail_host(&self, host: Arc<dyn MailToolHost>) {
        *self.mail.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
    }

    /// Install the mail access the proposal executor uses (approved suggestions only).
    pub fn set_action_host(&self, host: Arc<dyn MailActionHost>) {
        *self.actions.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
    }

    /// Install the read-only access the draft tools use.
    pub fn set_draft_host(&self, host: Arc<dyn DraftHost>) {
        *self.drafts.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
    }

    /// Install the rule-enable hook used when the user approves (or undoes) a rule-change suggestion.
    pub fn set_rule_hook(&self, hook: Arc<dyn proposals::RuleHook>) {
        *self.rule_hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// Install the calendar access used by conversations whose consent includes the calendar.
    pub fn set_calendar_hosts(&self, read: Arc<dyn CalendarHost>, write: Arc<dyn CalActionHost>) {
        *self.cal.lock().unwrap_or_else(|e| e.into_inner()) = Some(read);
        *self.cal_actions.lock().unwrap_or_else(|e| e.into_inner()) = Some(write);
    }

    #[cfg(test)]
    pub fn set_env_source(&self, f: Arc<dyn Fn() -> aichat::context::EnvInfo + Send + Sync>) {
        *self.env_source.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    fn cal_host(&self) -> Option<Arc<dyn CalendarHost>> {
        self.cal.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn cal_action_host(&self) -> Option<Arc<dyn CalActionHost>> {
        self.cal_actions.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn environment(&self) -> aichat::context::EnvInfo {
        if let Some(f) = self.env_source.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return f();
        }
        calhost::environment_info(chrono::Utc::now().timestamp(), &calhost::system_tz(), &calendar::avail::WorkHours::default())
    }

    fn draft_host(&self) -> Option<Arc<dyn DraftHost>> {
        self.drafts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn action_host(&self) -> Option<Arc<dyn MailActionHost>> {
        self.actions.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn mail_host(&self) -> Option<Arc<dyn MailToolHost>> {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    #[cfg(test)]
    fn with_limits(mut self, l: Limits) -> Self {
        self.limits = l;
        self
    }

    fn db<T>(&self, f: impl FnOnce(&mut Store) -> store::Result<T>) -> R<T> {
        let mut g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g).map_err(|_| DB_ERR.to_string())
    }

    /// A folder was renamed/moved on the server: the old names lose every grant (consent, saved scopes,
    /// MIRA-created-folder records, message refs). Never migrated to the new names. Returns how many grants
    /// were revoked.
    pub fn revoke_folder_grants(&self, account: &str, folders: &[String]) -> R<usize> {
        self.db(|s| s.revoke_folder_grants(account, folders))
    }

    async fn secret_get(&self, account: String) -> R<Option<String>> {
        let s = self.secrets.clone();
        tauri::async_runtime::spawn_blocking(move || s.get(&account))
            .await
            .map_err(|_| KEYCHAIN_ERR.to_string())?
            .map_err(|_| KEYCHAIN_ERR.to_string())
    }
    async fn secret_set(&self, account: String, value: String) -> R<()> {
        let s = self.secrets.clone();
        tauri::async_runtime::spawn_blocking(move || s.set(&account, &value))
            .await
            .map_err(|_| KEYCHAIN_ERR.to_string())?
            .map_err(|_| KEYCHAIN_ERR.to_string())
    }
    async fn secret_delete(&self, account: String) -> R<()> {
        let s = self.secrets.clone();
        tauri::async_runtime::spawn_blocking(move || s.delete(&account))
            .await
            .map_err(|_| KEYCHAIN_ERR.to_string())?
            .map_err(|_| KEYCHAIN_ERR.to_string())
    }

    // ---- startup / purge -------------------------------------------------------------------

    /// Apply the retention settings (local deletes only; no network).
    pub fn startup_purge(&self) {
        let _ = self.purge_now();
        // A suggestion left "executing" by a crash or quit is settled from its log.
        let now = system_clock()();
        let _ = self.db(|s| s.recover_interrupted(now));
        // An auto-reply that was reserved but not confirmed when the app quit frees its slot.
        let _ = self.db(|s| s.settle_pending_replies());
    }

    /// Shared handles for local automation (rules, auto-reply). It uses the same `ai.db` and the same
    /// executor, but nothing of the chat engine.
    pub fn store_handle(&self) -> Arc<Mutex<Store>> {
        self.store.clone()
    }

    pub fn busy_handle(&self) -> proposals::Busy {
        self.busy.clone()
    }

    /// A mail account was removed: its rules, run history and auto-reply settings go too.
    pub fn forget_account_automation(&self, account: &str) {
        let _ = self.db(|s| {
            s.purge_rules_account(account)?;
            s.purge_autoreply_account(account)
        });
    }

    pub fn purge_now(&self) -> R<PurgeReport> {
        let now = system_clock()();
        let cfg = self.get_settings()?.purge;
        let rep = self.db(|s| s.purge_now(&to_store_purge(&cfg), now))?;
        Ok(PurgeReport { egress_deleted: rep.egress_deleted, conversations_deleted: rep.conversations_deleted })
    }

    /// Called when a mail account is removed: drops AI conversations/refs tied to it if the
    /// `purgeOnAccountRemoval` setting is on.
    pub fn purge_account(&self, account: &str) {
        if self.get_settings().map(|s| s.purge.purge_on_account_removal).unwrap_or(true) {
            let _ = self.db(|s| s.purge_account(account));
        }
    }

    // ---- settings --------------------------------------------------------------------------

    pub fn get_settings(&self) -> R<Settings> {
        self.db(|s| {
            let mut out = Settings::default();
            if let Some(j) = s.get_setting("limits")? {
                if let Ok(v) = serde_json::from_str::<Value>(&j) {
                    let g = |k: &str, d: u64| v.get(k).and_then(Value::as_u64).unwrap_or(d);
                    out.per_turn_tokens = g("perTurnTokens", out.per_turn_tokens);
                    out.per_conversation_tokens = g("perConversationTokens", out.per_conversation_tokens);
                    out.per_day_tokens = g("perDayTokens", out.per_day_tokens);
                }
            }
            if let Some(j) = s.get_setting("redaction")? {
                if let Ok(r) = serde_json::from_str::<Redaction>(&j) {
                    out.default_redaction = r;
                }
            }
            let p = s.purge_settings()?;
            out.purge = PurgeCfg {
                egress_retention_days: p.egress_retention_days,
                conversation_retention_days: p.conversation_retention_days,
                purge_on_account_removal: p.purge_on_account_removal,
            };
            Ok(out)
        })
    }

    pub fn set_settings(&self, s: Settings) -> R<Settings> {
        if !(1_000..=2_000_000).contains(&s.per_turn_tokens) {
            return Err("The per-request token limit must be between 1,000 and 2,000,000.".into());
        }
        if s.per_conversation_tokens < s.per_turn_tokens || s.per_conversation_tokens > 100_000_000 {
            return Err("The per-conversation token limit must be at least the per-request limit (and at most 100,000,000).".into());
        }
        if s.per_day_tokens < s.per_turn_tokens || s.per_day_tokens > 1_000_000_000 {
            return Err("The daily token limit must be at least the per-request limit (and at most 1,000,000,000).".into());
        }
        for d in [s.purge.egress_retention_days, s.purge.conversation_retention_days].into_iter().flatten() {
            if d > 3650 {
                return Err("Retention can be at most 3650 days; choose \"keep\" instead.".into());
            }
        }
        let limits = json!({"perTurnTokens": s.per_turn_tokens, "perConversationTokens": s.per_conversation_tokens, "perDayTokens": s.per_day_tokens});
        let red = serde_json::to_string(&s.default_redaction).map_err(|_| DB_ERR.to_string())?;
        self.db(|st| {
            st.set_setting("limits", &limits.to_string())?;
            st.set_setting("redaction", &red)?;
            st.set_purge_settings(&to_store_purge(&s.purge))
        })?;
        self.get_settings()
    }

    fn caps(&self) -> Caps {
        let s = self.get_settings().unwrap_or_default();
        Caps { per_turn: s.per_turn_tokens, per_conversation: s.per_conversation_tokens, per_day: s.per_day_tokens }
    }

    // ---- profiles --------------------------------------------------------------------------

    pub fn list_profiles(&self) -> R<Vec<Profile>> {
        Ok(self.db(|s| s.list_profiles())?.iter().map(profile_to_wire).collect())
    }

    pub async fn save_profile(&self, mut p: Profile, api_key: Option<String>) -> R<Profile> {
        validate_profile(&mut p)?;
        let base = http::validate_base_url(&p.base_url).map_err(|e| e.message)?;
        let origin = base.origin();
        let existing = self.db(|s| s.get_profile(&p.id))?;

        enum KeyAction {
            Keep,
            Clear,
            Set(String),
        }
        let action = match api_key.as_deref().map(str::trim) {
            None => KeyAction::Keep,
            Some("") => KeyAction::Clear,
            Some(k) => {
                validate_key(k)?;
                KeyAction::Set(k.to_string())
            }
        };
        if let (KeyAction::Keep, Some(ex)) = (&action, &existing) {
            if ex.has_key && !ex.key_host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(&origin)) {
                return Err(KEY_REENTER.into());
            }
        }

        let extra = extra_json(&p);
        let input = ProfileInput { id: p.id.clone(), name: p.name.clone(), dialect: p.kind.clone(), base_url: base.url.clone(), model: p.model.clone(), extra_json: extra };
        let now = system_clock()();
        self.db(|s| s.save_profile(&input, now))?;

        match action {
            KeyAction::Keep => {}
            KeyAction::Clear => {
                self.secret_delete(key_account(&p.id)).await?;
                self.db(|s| s.mark_key_cleared(&p.id, now))?;
            }
            KeyAction::Set(k) => {
                // The profile validated above; only now does the key reach the Keychain.
                if let Err(e) = self.secret_set(key_account(&p.id), k).await {
                    if existing.is_none() {
                        let _ = self.db(|s| s.delete_profile(&p.id));
                    }
                    return Err(e);
                }
                self.db(|s| s.mark_key_saved(&p.id, now))?;
            }
        }
        let saved = self.db(|s| s.get_profile(&p.id))?.ok_or_else(|| DB_ERR.to_string())?;
        Ok(profile_to_wire(&saved))
    }

    pub async fn delete_profile(&self, id: &str) -> R<()> {
        self.secret_delete(key_account(id)).await?;
        self.db(|s| s.delete_profile(id))?;
        Ok(())
    }

    /// Check an endpoint. Sends no mail data and persists nothing. Never rejects: problems are
    /// reported as `ok: false` with a friendly message.
    pub async fn test_profile(&self, p: Profile, api_key: Option<String>) -> TestResult {
        self.run_test(p, api_key, false).await
    }

    /// Fetch the model list only (one `GET /models`, never a chat request). Used when a key is typed.
    /// Anthropic-kind profiles contact nothing and return an empty list.
    pub async fn list_models(&self, p: Profile, api_key: Option<String>) -> TestResult {
        self.run_test(p, api_key, true).await
    }

    async fn run_test(&self, p: Profile, api_key: Option<String>, models_only: bool) -> TestResult {
        match self.test_inner(p, api_key, models_only).await {
            Ok(r) => r,
            Err(message) => TestResult { ok: false, latency_ms: 0, models: vec![], supports_tools: false, message },
        }
    }

    async fn test_inner(&self, mut p: Profile, api_key: Option<String>, models_only: bool) -> R<TestResult> {
        if p.id.is_empty() {
            p.id = "test".into();
        }
        // A test may run with the model still empty: the endpoint's model list then picks one (below).
        let model_was_empty = p.model.trim().is_empty();
        if model_was_empty {
            p.model = "pending".into();
        }
        validate_profile(&mut p)?;
        let base = http::validate_base_url(&p.base_url).map_err(|e| e.message)?;
        let dialect = parse_kind(&p.kind)?;
        // Key: the one typed in the form, else the saved one (only if it belongs to this origin).
        let key: Option<String> = match api_key.as_deref().map(str::trim) {
            Some(k) if !k.is_empty() => {
                validate_key(k)?;
                Some(k.to_string())
            }
            _ => {
                let saved = self.db(|s| s.get_profile(&p.id))?;
                match saved {
                    Some(ex) if ex.has_key => {
                        if !ex.key_host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(&base.origin())) {
                            return Err(KEY_REENTER.into());
                        }
                        self.secret_get(key_account(&p.id)).await?
                    }
                    _ => None,
                }
            }
        };
        let auth = match (dialect, key.as_deref()) {
            (_, None) => Auth::none(),
            (Dialect::Anthropic, Some(k)) => Auth::x_api_key(k),
            (Dialect::OpenAiChat, Some(k)) => Auth::bearer(k),
        };
        let limits = Limits {
            connect_timeout: self.limits.connect_timeout,
            idle_timeout: self.limits.idle_timeout.min(Duration::from_secs(25)),
            total_timeout: self.limits.total_timeout.min(Duration::from_secs(45)),
            max_body_bytes: self.limits.max_body_bytes,
        };
        let client = http::build_client_with(&base, &limits).map_err(|e| e.message)?;
        let (_keep_open, stop) = tokio::sync::watch::channel(false);

        // 1. model list (OpenAI-style endpoints only; a missing list is not an error)
        let mut models: Vec<String> = Vec::new();
        if dialect == Dialect::OpenAiChat {
            match http::send_get(&client, &base, "/models", &[], &auth, stop.clone(), &limits).await {
                Ok(resp) => {
                    let body = http::read_body(resp, stop.clone(), &limits, &base).await.map_err(|e| e.message)?;
                    models = parse_models(&body);
                }
                Err(e) if matches!(e.status, Some(404 | 405 | 501)) => {}
                Err(e) => return Err(e.message),
            }
        }

        if models_only {
            let message = if models.is_empty() { "No model list available from this endpoint.".to_string() } else { format!("{} model{} listed.", models.len(), if models.len() == 1 { "" } else { "s" }) };
            return Ok(TestResult { ok: true, latency_ms: 0, models, supports_tools: false, message });
        }
        if model_was_empty {
            match models.first() {
                Some(m) => p.model = m.clone(),
                None => return Err("This endpoint did not list any chat models. Type a model name.".into()),
            }
        }
        // 2. tiny streamed ping with one dummy tool
        let opts = options_for(&p, &base, dialect);
        let ping = |with_tool: bool| ChatRequest {
            model: p.model.clone(),
            system: None,
            messages: vec![ChatMessage::user_text("Reply with the single word: ok")],
            tools: if with_tool {
                vec![ToolSpec {
                    name: "noop".into(),
                    description: "Does nothing. Never call it.".into(),
                    parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
                }]
            } else {
                vec![]
            },
            max_output_tokens: 8,
        };
        let t0 = Instant::now();
        let mut supports_tools = true;
        let first = run_ping(&client, &base, &auth, &opts, &ping(true), &stop, &limits).await;
        if let Err(e) = first {
            if e.kind == ErrorKind::BadRequest {
                // Maybe the tool is what it rejected: retry without it.
                run_ping(&client, &base, &auth, &opts, &ping(false), &stop, &limits).await.map_err(|_| e.message.clone())?;
                supports_tools = false;
            } else {
                return Err(e.message);
            }
        }
        let latency_ms = t0.elapsed().as_millis() as u64;
        let mut msg = format!("Connected to {}.", engine::host_label(&base));
        if !models.is_empty() {
            msg.push_str(&format!(" {} model{} listed.", models.len(), if models.len() == 1 { "" } else { "s" }));
            if !models.iter().any(|m| m == &p.model) {
                msg.push_str(" The model you entered is not in that list; check the spelling.");
            }
        }
        msg.push_str(if supports_tools { " Tool calling accepted." } else { " This endpoint did not accept tool definitions, so the assistant will be limited to plain chat." });
        Ok(TestResult { ok: true, latency_ms, models, supports_tools, message: msg })
    }

    // ---- conversations ---------------------------------------------------------------------

    pub fn new_conversation(&self, profile_id: &str, consent: Consent) -> R<Conversation> {
        check_consent_limits(&consent)?;
        let prof = self
            .db(|s| s.get_profile(profile_id))?
            .ok_or_else(|| "That AI profile no longer exists. Pick another one.".to_string())?;
        let base = http::validate_base_url(&prof.base_url).map_err(|e| e.message)?;
        let mut accounts: Vec<String> = consent.scopes.iter().filter(|s| s.allowed).map(|s| s.account.clone()).collect();
        accounts.sort();
        accounts.dedup();
        let consent_json = serde_json::to_string(&consent).map_err(|_| DB_ERR.to_string())?;
        let now = system_clock()();
        let nc = NewConversation {
            title: DEFAULT_TITLE.into(),
            profile_id: Some(prof.id.clone()),
            endpoint_host: engine::host_label(&base),
            model: prof.model.clone(),
            consent_json,
            accounts,
        };
        let id = self.db(|s| s.create_conversation(&nc, now))?;
        let c = self.db(|s| s.get_conversation(id))?.ok_or_else(|| DB_ERR.to_string())?;
        Ok(conv_to_wire(&c))
    }

    pub fn list_conversations(&self) -> R<Vec<Conversation>> {
        Ok(self.db(|s| s.list_conversations())?.iter().map(conv_to_wire).collect())
    }

    pub fn get_conversation(&self, id: i64) -> R<ConversationWithMessages> {
        let (c, msgs) = self.db(|s| {
            let c = s.get_conversation(id)?;
            let m = if c.is_some() { s.list_messages(id)? } else { vec![] };
            Ok((c, m))
        })?;
        let c = c.ok_or_else(|| "That conversation no longer exists.".to_string())?;
        let runs = self.db(|s| s.list_tool_runs(id))?;
        let messages = fold_messages(&msgs, &runs);
        let consent = self.effective_consent(id)?;
        Ok(ConversationWithMessages { conversation: conv_to_wire(&c), messages, consent })
    }

    /// The consent in force for a conversation: the saved one plus folders MIRA created in it (approved
    /// create-folder / move-to-new-folder cards) whose account is still ticked, shown ticked.
    pub fn effective_consent(&self, conv_id: i64) -> R<Consent> {
        let conv = self.db(|s| s.get_conversation(conv_id))?.ok_or_else(|| "That conversation no longer exists.".to_string())?;
        let mut consent: Consent = serde_json::from_str(&conv.consent_json).unwrap_or_default();
        for (account, folder) in self.db(|s| s.list_conv_folders(conv_id))? {
            if !consent.scopes.iter().any(|x| x.allowed && x.account == account) {
                continue;
            }
            match consent.scopes.iter_mut().find(|x| x.account == account && x.folder == folder) {
                Some(x) => x.allowed = true,
                None => consent.scopes.push(Scope { account, folder, allowed: true }),
            }
        }
        Ok(consent)
    }

    /// "Edit permissions" (user action only; the model has no tool for this). Validates `new` like a new
    /// conversation plus existence of the ticked accounts / folders / calendars, refuses while a turn or an
    /// approved suggestion is running, then in ONE transaction saves it, clears the `conv_folders` rows
    /// (the saved consent now holds them) and appends a visible `notice` line. Earlier messages and
    /// suggestions are untouched; narrowing applies at once because every turn and every approval re-reads
    /// the stored consent. Nothing is sent anywhere. A change that alters nothing writes nothing.
    pub async fn update_consent(&self, conv_id: i64, new: Consent) -> R<ConversationWithMessages> {
        check_consent_limits(&new)?;
        let conv = self.db(|s| s.get_conversation(conv_id))?.ok_or_else(|| "That conversation no longer exists.".to_string())?;
        // Holding the conversation's turn slot keeps a new turn from starting while we validate and write.
        let _ticket = self.inflight.begin(conv_id).map_err(|_| BUSY_PERMS.to_string())?;
        self.ensure_no_running_proposal(conv_id)?;
        // Slow IMAP lookups happen BEFORE the global gate (it would otherwise block every Approve in every
        // conversation), bounded by a timeout that fails closed. The gate is then held only for cheap checks.
        if let Some(host) = self.mail_host() {
            let check = async {
                let accounts = host.accounts().await?;
                for acc in new.scopes.iter().filter(|s| s.allowed).map(|s| s.account.as_str()).collect::<std::collections::BTreeSet<_>>() {
                    if !accounts.iter().any(|a| a == acc) {
                        return Err("One of the ticked mail accounts doesn't exist any more. Reopen the dialog and try again.".to_string());
                    }
                    let folders = host.folders(acc).await?;
                    for s in new.scopes.iter().filter(|s| s.allowed && s.account == acc) {
                        if !folders.iter().any(|f| f.name == s.folder) {
                            return Err("One of the ticked folders doesn't exist any more. Reopen the dialog and try again.".to_string());
                        }
                    }
                }
                Ok(())
            };
            tokio::time::timeout(self.consent_check_timeout, check)
                .await
                .map_err(|_| "The mail server took too long to answer, so permissions weren't changed. Try again.".to_string())??;
        } else if new.scopes.iter().any(|s| s.allowed) {
            return Err("Mail isn't available right now, so permissions can't be changed.".into());
        }
        // From the busy check to the write no approval may claim a proposal and read the old consent.
        let _gate = self.consent_gate.lock().await;
        self.ensure_no_running_proposal(conv_id)?;
        let ticked_cals: Vec<&str> = new.calendar.calendars.iter().filter(|c| c.allowed).map(|c| c.id.as_str()).collect();
        if new.calendar.enabled && !ticked_cals.is_empty() {
            let host = self.cal_host().ok_or_else(|| "Calendars aren't available right now, so permissions can't be changed.".to_string())?;
            let known = host.calendars()?;
            if ticked_cals.iter().any(|id| !known.iter().any(|c| c.id == *id)) {
                return Err("One of the ticked calendars doesn't exist any more. Reopen the dialog and try again.".into());
            }
        }
        // Nothing may have started meanwhile.
        self.ensure_no_running_proposal(conv_id)?;
        let old = self.effective_consent(conv_id)?;
        let Some(notice) = describe_consent_change(&old, &new) else {
            return self.get_conversation(conv_id);
        };
        let mut accounts = conv.accounts.clone();
        accounts.extend(new.scopes.iter().filter(|s| s.allowed).map(|s| s.account.clone()));
        accounts.sort();
        accounts.dedup();
        let consent_json = serde_json::to_string(&new).map_err(|_| DB_ERR.to_string())?;
        let parts = serde_json::to_string(&[Part::text(notice)]).map_err(|_| DB_ERR.to_string())?;
        let now = system_clock()();
        self.db(|s| s.update_consent(conv_id, &consent_json, &accounts, &parts, now))?;
        self.get_conversation(conv_id)
    }

    fn ensure_no_running_proposal(&self, conv_id: i64) -> R<()> {
        let props = self.db(|s| s.list_proposals(Some(conv_id), 1000))?;
        let busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        if props.iter().any(|p| p.state == proposals::State::Executing || busy.contains(&p.id)) {
            return Err(BUSY_PERMS.into());
        }
        Ok(())
    }

    pub fn delete_conversation(&self, id: i64) -> R<()> {
        self.inflight.cancel(id);
        self.db(|s| s.delete_conversation(id))?;
        Ok(())
    }

    // ---- turns -----------------------------------------------------------------------------

    /// Run one turn. Problems found before anything starts reject; once the turn is running,
    /// failures are delivered to `sink` as an error event and this returns `Ok`.
    pub async fn send(&self, conv_id: i64, text: String, sink: &dyn EventSink) -> R<()> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("Type a message first.".into());
        }
        let (conv, prof) = self.db(|s| {
            let c = s.get_conversation(conv_id)?;
            let p = match c.as_ref().and_then(|c| c.profile_id.clone()) {
                Some(id) => s.get_profile(&id)?,
                None => None,
            };
            Ok((c, p))
        })?;
        let conv = conv.ok_or_else(|| "That conversation no longer exists.".to_string())?;
        let prof = prof.ok_or_else(|| "The AI profile for this conversation was deleted. Start a new conversation with another profile.".to_string())?;
        if store::needs_key_reentry(&prof) {
            return Err(KEY_REENTER.into());
        }
        let base = http::validate_base_url(&prof.base_url).map_err(|e| e.message)?;
        // Consent is per endpoint: the profile may have been edited since this conversation began.
        if engine::host_label(&base) != conv.endpoint_host || prof.model != conv.model {
            return Err("This profile's server or model changed after this conversation started. Consent applies to one endpoint, so start a new conversation.".into());
        }
        let ticket = self.inflight.begin(conv_id).map_err(|e| e.message)?;
        let key = if prof.has_key {
            Some(self.secret_get(key_account(&prof.id)).await?.ok_or_else(|| "The saved API key could not be found in the Keychain. Enter it again in Settings > AI.".to_string())?)
        } else {
            None
        };
        let rt = runtime_of(&prof, &base)?;
        // Re-read the consent now that this turn holds the slot: an Edit permissions save that finished
        // between the first read and the ticket is honoured for this turn.
        let consent_json = self.db(|s| s.get_conversation(conv_id))?.ok_or_else(|| "That conversation no longer exists.".to_string())?.consent_json;
        let redactions_json = serde_json::from_str::<Value>(&consent_json)
            .ok()
            .and_then(|v| v.get("redaction").cloned())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".into());
        let consent: Consent = serde_json::from_str(&consent_json).unwrap_or_default();
        // Proposal tools exist only when tools are on AND the user allowed suggestions for this conversation.
        let actions = if consent.tools && consent.suggest { self.action_host() } else { None };
        // Draft tools follow the same consent: tools AND suggest.
        let draft_host = if consent.tools && consent.suggest { self.draft_host() } else { None };
        let calendar = if consent.tools && consent.calendar.enabled {
            self.cal_host().map(|host| CalSetup {
                host,
                calendars: consent.calendar.calendars.iter().filter(|c| c.allowed).map(|c| c.id.clone()).collect(),
                propose: consent.suggest && self.cal_action_host().is_some(),
            })
        } else {
            None
        };
        let tools = if consent.tools { self.mail_host().map(|host| tool_setup(host, &consent, actions, draft_host, calendar)) } else { None };
        let env = consent.tools.then(|| self.environment());
        let ctx = TurnCtx {
            store: self.store.clone(),
            conv_id,
            profile: rt,
            api_key: key,
            key_origin: prof.key_host.clone(),
            caps: self.caps(),
            limits: self.limits.clone(),
            clock: system_clock(),
            cancel: ticket.cancel.clone(),
            redactions_json,
            tools,
            env,
        };
        let _ = engine::run_turn(ctx, &text, sink).await; // outcome already delivered through the sink
        drop(ticket);
        Ok(())
    }

    // ---- proposals (P2) ---------------------------------------------------------------------

    fn consent_scopes(&self, conv_id: i64) -> R<Vec<ToolScope>> {
        let consent = self.effective_consent(conv_id)?;
        Ok(consent.scopes.iter().filter(|s| s.allowed).map(|s| ToolScope { account: s.account.clone(), folder: s.folder.clone() }).collect())
    }

    fn executor(&self) -> R<Executor> {
        let host = self.action_host().ok_or_else(|| "Changing mail isn't available right now.".to_string())?;
        let ex = Executor::new(self.store.clone(), host, system_clock(), self.busy.clone());
        Ok(match self.rule_hook.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(h) => ex.with_rule_hook(h),
            None => ex,
        })
    }

    fn summary_of(st: &mut Store, p: &proposals::Proposal) -> store::Result<ProposalSummary> {
        let counts = st.proposal_counts(p.id)?;
        let log = st.list_log(p.id)?;
        Ok(ProposalSummary {
            id: p.id,
            conv_id: p.conv_id,
            kind: p.kind.as_str().into(),
            state: p.state.as_str().into(),
            title: p.title.clone(),
            reason: p.reason.clone(),
            warnings: p.warnings.clone(),
            error: p.error.clone(),
            created_at: p.created_at,
            updated_at: p.updated_at,
            expires_at: p.expires_at,
            counts: ProposalCounts {
                total: counts.total,
                selected: counts.selected,
                done: counts.done,
                failed: counts.failed,
                skipped: counts.skipped,
                noop: counts.noop,
                undone: counts.undone,
                undo_failed: counts.undo_failed,
                unknown: counts.unknown,
            },
            undoable: if p.kind.is_calendar() { calexec::cal_undoable(st, p) } else { proposals::undoable(p, &log) },
            event: p.params.cal.as_ref().filter(|_| p.kind.is_calendar()).map(|c| EventCard {
                calendar_name: c.calendar_name.clone(),
                rows: c.card.iter().map(|r| EventCardRow { label: r.label.clone(), before: r.before.clone(), after: r.after.clone() }).collect(),
                notify: c.notify,
                undo_note: c.undo_note.clone(),
            }),
        })
    }

    /// Newest first. `conv_id: None` = the activity log across all conversations.
    pub fn list_proposals(&self, conv_id: Option<i64>, limit: u32) -> R<Vec<ProposalSummary>> {
        let now = system_clock()();
        self.db(|s| {
            s.expire_stale(now)?;
            let ps = s.list_proposals(conv_id, limit.clamp(1, 500))?;
            ps.iter().map(|p| Self::summary_of(s, p)).collect()
        })
    }

    pub fn get_proposal(&self, id: i64) -> R<ProposalDetail> {
        let now = system_clock()();
        let r = self.db(|s| {
            s.expire_stale(now)?;
            let Some(p) = s.get_proposal(id)? else { return Ok(None) };
            let summary = Self::summary_of(s, &p)?;
            let items = s
                .proposal_items(id)?
                .into_iter()
                .map(|i| ProposalItem {
                    idx: i.idx,
                    account: i.account,
                    folder: i.folder,
                    sender: i.sender,
                    subject: i.subject,
                    seen: i.seen,
                    flagged: i.flagged,
                    to_folder: i.to_folder,
                    selected: i.selected,
                    status: i.status.as_str().into(),
                    error: i.error,
                })
                .collect();
            Ok(Some(ProposalDetail { summary, items }))
        })?;
        r.ok_or_else(|| proposals::ProposalError::NotFound.to_string())
    }

    /// Small state snapshot for the `ai://proposal` event.
    pub fn proposal_event(&self, id: i64) -> Option<ProposalEvent> {
        self.db(|s| {
            let Some(p) = s.get_proposal(id)? else { return Ok(None) };
            let c = s.proposal_counts(id)?;
            Ok(Some(ProposalEvent {
                id,
                conv_id: p.conv_id,
                state: p.state.as_str().into(),
                counts: ProposalCounts { total: c.total, selected: c.selected, done: c.done, failed: c.failed, skipped: c.skipped, noop: c.noop, undone: c.undone, undo_failed: c.undo_failed, unknown: c.unknown },
            }))
        })
        .ok()
        .flatten()
    }

    /// Approve and execute. Only this call (and `undo_proposal`) can change mail. Returns the result and
    /// the `(account, folder)` pairs whose contents changed.
    ///
    /// `selected` = the `idx` of the rows to run. Rows not named are NOT executed (fail closed). Refused
    /// when the user has since turned suggestions off for the conversation.
    pub async fn approve_proposal(&self, id: i64, selected: Vec<i64>, progress: &(dyn Fn(i64) + Send + Sync)) -> R<(ApproveResult, Vec<(String, String)>)> {
        let prop = self.db(|s| s.get_proposal(id))?.ok_or_else(|| proposals::ProposalError::NotFound.to_string())?;
        let conv_id = prop.conv_id;
        // Claim FIRST, read the consent SECOND (both under the gate): an "Edit permissions" save that came
        // earlier is seen here; one that comes later is refused while the claim is held.
        let gate = self.consent_gate.lock().await;
        let calendar_ex = if prop.kind.is_calendar() { Some(self.cal_action_host().map(|h| CalExecutor::new(self.store.clone(), h, system_clock(), self.busy.clone()))) } else { None };
        let claimed = if prop.kind.is_calendar() { None } else { Some(self.executor()?) };
        let guard = match (&claimed, &calendar_ex) {
            (Some(ex), _) => Some(ex.claim(id).map_err(|e| e.to_string())?),
            (None, Some(Some(ex))) => Some(ex.claim(id).map_err(|e| e.to_string())?),
            _ => None,
        };
        let conv = self.db(|s| s.get_conversation(conv_id))?.ok_or_else(|| "That conversation no longer exists.".to_string())?;
        let consent: Consent = serde_json::from_str(&conv.consent_json).unwrap_or_default();
        if !(consent.tools && consent.suggest) {
            return Err("Suggestions are turned off for this conversation, so this one can't be approved. Turn them back on in the conversation's settings first.".to_string());
        }
        let scopes = self.consent_scopes(conv_id)?;
        let mut allowed: Vec<String> = vec![];
        if prop.kind.is_calendar() {
            if !consent.calendar.enabled {
                return Err("The calendar is turned off for this conversation, so this one can't be approved.".to_string());
            }
            allowed = consent.calendar.calendars.iter().filter(|c| c.allowed).map(|c| c.id.clone()).collect();
            if guard.is_none() {
                return Err("Changing the calendar isn't available right now.".to_string());
            }
        }
        drop(gate);
        #[cfg(test)]
        {
            let hook = self.after_claim_hook.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(h) = hook {
                h().await;
            }
        }
        if let Some(Some(ex)) = calendar_ex {
            let guard = guard.ok_or_else(|| DB_ERR.to_string())?;
            let rep = ex.approve_claimed(guard, id, &allowed, &scopes, progress).await.map_err(|e| e.to_string())?;
            let proposal = self.get_proposal(id)?;
            let ok = u32::from(rep.state == proposals::State::Done);
            return Ok((ApproveResult { proposal, ok, failed: 1 - ok, skipped: 0, noop: 0, folders_changed: false, note: rep.summary }, vec![]));
        }
        let (ex, guard) = (claimed.ok_or_else(|| DB_ERR.to_string())?, guard.ok_or_else(|| DB_ERR.to_string())?);
        let rep = ex.approve_claimed(guard, id, &selected, &scopes, progress).await.map_err(|e| e.to_string())?;
        let proposal = self.get_proposal(id)?;
        let res = ApproveResult { proposal, ok: rep.ok as u32, failed: rep.failed as u32, skipped: rep.skipped as u32, noop: rep.noop as u32, folders_changed: rep.folders_changed, note: String::new() };
        Ok((res, rep.affected))
    }

    pub fn reject_proposal(&self, id: i64) -> R<ProposalDetail> {
        proposals::reject(&self.store, id, system_clock()()).map_err(|e| e.to_string())?;
        self.get_proposal(id)
    }

    pub async fn undo_proposal(&self, id: i64, progress: &(dyn Fn(i64) + Send + Sync)) -> R<(UndoResult, Vec<(String, String)>)> {
        if let Some(p) = self.db(|s| s.get_proposal(id))?.filter(|p| p.kind.is_calendar()) {
            let conv = self.db(|s| s.get_conversation(p.conv_id))?.ok_or_else(|| "That conversation no longer exists.".to_string())?;
            let consent: Consent = serde_json::from_str(&conv.consent_json).unwrap_or_default();
            let allowed: Vec<String> = consent.calendar.calendars.iter().filter(|c| c.allowed).map(|c| c.id.clone()).collect();
            let host = self.cal_action_host().ok_or_else(|| "Changing the calendar isn't available right now.".to_string())?;
            let ex = CalExecutor::new(self.store.clone(), host, system_clock(), self.busy.clone());
            let rep = ex.undo(id, &allowed, progress).await.map_err(|e| e.to_string())?;
            let proposal = self.get_proposal(id)?;
            return Ok((UndoResult { proposal, undone: rep.undone as u32, failed: rep.failed as u32, errors: rep.errors }, vec![]));
        }
        let ex = self.executor()?;
        let rep = ex.undo(id, progress).await.map_err(|e| e.to_string())?;
        let proposal = self.get_proposal(id)?;
        Ok((UndoResult { proposal, undone: rep.undone as u32, failed: rep.failed as u32, errors: rep.errors }, rep.affected))
    }

    // ---- drafts (P3) ------------------------------------------------------------------------
    // Nothing in this block can send mail. A draft is text + recipients computed by the app; the user opens it
    // in Compose and presses Send there.

    fn draft_summary(d: &drafts::Draft) -> DraftSummary {
        DraftSummary {
            id: d.id,
            conv_id: d.conv_id,
            kind: d.kind.as_str().into(),
            state: d.state.as_str().into(),
            title: d.title.clone(),
            account: d.account.clone(),
            to: d.to.clone(),
            cc: d.cc.clone(),
            subject: d.subject.clone(),
            body: d.body.clone(),
            quote: d.quote.clone(),
            reason: d.reason.clone(),
            warnings: d.warnings.clone(),
            new_addresses: d.new_addresses.clone(),
            created_at: d.created_at,
            updated_at: d.updated_at,
        }
    }

    pub fn list_drafts(&self, conv_id: Option<i64>, limit: u32) -> R<Vec<DraftSummary>> {
        let v = self.db(|s| s.list_drafts(conv_id, limit.clamp(1, 500)))?;
        Ok(v.iter().map(Self::draft_summary).collect())
    }

    pub fn get_draft(&self, id: i64) -> R<DraftSummary> {
        let d = self.db(|s| s.get_draft(id))?.ok_or_else(|| drafts::DraftError::NotFound.to_string())?;
        Ok(Self::draft_summary(&d))
    }

    /// `Proposed | Opened -> Opened`; returns what Compose is prefilled with. A dismissed draft is refused.
    pub fn open_draft(&self, id: i64) -> R<(DraftPrefill, DraftEvent)> {
        let d = self.db(|s| Ok(s.mark_draft_opened(id, system_clock()())))?.map_err(|e| e.to_string())?;
        let prefill = DraftPrefill {
            id: d.id,
            account: d.account.clone(),
            to: d.to.clone(),
            cc: d.cc.clone(),
            bcc: vec![],
            subject: d.subject.clone(),
            text: drafts::compose_text(&d.body, &d.quote),
            in_reply_to: d.in_reply_to.clone().map(|m| DraftReplyTo { message_id: m, references: (!d.references.is_empty()).then(|| d.references.clone()) }),
            ai_draft: true,
            warnings: d.warnings.clone(),
        };
        let ev = DraftEvent { id: d.id, conv_id: d.conv_id, state: d.state.as_str().into() };
        Ok((prefill, ev))
    }

    pub fn dismiss_draft(&self, id: i64) -> R<(DraftSummary, DraftEvent)> {
        let d = self.db(|s| Ok(s.mark_draft_dismissed(id, system_clock()())))?.map_err(|e| e.to_string())?;
        let ev = DraftEvent { id: d.id, conv_id: d.conv_id, state: d.state.as_str().into() };
        Ok((Self::draft_summary(&d), ev))
    }

    pub fn cancel(&self, conv_id: i64) {
        self.inflight.cancel(conv_id);
    }

    pub fn egress(&self, conv_id: i64, limit: u32) -> R<Vec<EgressRow>> {
        let rows = self.db(|s| s.list_egress(Some(conv_id), limit.clamp(1, 200)))?;
        Ok(rows
            .into_iter()
            .map(|r| EgressRow {
                id: r.id,
                round: r.round,
                at: r.ts,
                host: r.endpoint_host,
                model: r.model,
                bytes: r.bytes,
                est_tokens: r.est_tokens,
                request_json: r.request_json,
                items: serde_json::from_str(&r.items_json).unwrap_or_default(),
            })
            .collect())
    }
}

// ------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------

fn to_store_purge(c: &PurgeCfg) -> PurgeSettings {
    PurgeSettings {
        egress_retention_days: c.egress_retention_days,
        conversation_retention_days: c.conversation_retention_days,
        purge_on_account_removal: c.purge_on_account_removal,
    }
}

fn parse_kind(k: &str) -> R<Dialect> {
    match k {
        "openai_chat" => Ok(Dialect::OpenAiChat),
        "anthropic" => Ok(Dialect::Anthropic),
        _ => Err("Choose a provider type: OpenAI-compatible or Anthropic.".into()),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mix = (nanos as u64) ^ (N.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ u64::from(std::process::id());
    format!("p-{mix:016x}")
}

/// Validate and normalise a profile in place (generates the id when empty).
fn validate_profile(p: &mut Profile) -> R<()> {
    if p.id.is_empty() {
        p.id = new_id();
    }
    if !valid_id(&p.id) {
        return Err("Internal error: invalid profile id.".into());
    }
    p.name = p.name.trim().to_string();
    if p.name.is_empty() || p.name.chars().count() > 80 {
        return Err("Give the profile a name (up to 80 characters).".into());
    }
    parse_kind(&p.kind)?;
    p.model = p.model.trim().to_string();
    if p.model.is_empty() || p.model.chars().count() > 200 || p.model.chars().any(char::is_control) {
        return Err("Choose or enter a model name.".into());
    }
    if p.max_output_tokens == 0 {
        p.max_output_tokens = DEFAULT_MAX_OUTPUT;
    }
    if p.max_output_tokens > MAX_OUTPUT_LIMIT {
        return Err(format!("The maximum reply length can be at most {MAX_OUTPUT_LIMIT} tokens."));
    }
    p.anthropic_version = p.anthropic_version.take().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if let Some(v) = &p.anthropic_version {
        if v.len() > 32 || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            return Err("The anthropic-version value looks wrong (expected something like 2023-06-01).".into());
        }
    }
    Ok(())
}

fn validate_key(k: &str) -> R<()> {
    if k.len() > 4096 || k.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("That API key contains spaces or line breaks. Paste just the key.".into());
    }
    Ok(())
}

fn extra_json(p: &Profile) -> String {
    let mut m = serde_json::Map::new();
    m.insert("maxOutputTokens".into(), json!(p.max_output_tokens));
    if let Some(v) = &p.anthropic_version {
        m.insert("anthropicVersion".into(), json!(v));
    }
    if let Some(t) = p.tested_at {
        m.insert("testedAt".into(), json!(t));
    }
    if let Some(ok) = p.test_ok {
        m.insert("testOk".into(), json!(ok));
    }
    Value::Object(m).to_string()
}

fn profile_to_wire(p: &store::Profile) -> Profile {
    let extra: Value = serde_json::from_str(&p.extra_json).unwrap_or(Value::Null);
    let warnings = http::validate_base_url(&p.base_url)
        .map(|b| {
            b.warnings
                .iter()
                .map(|w| Warning {
                    kind: match w {
                        UrlWarning::CleartextRemote => "cleartextRemote".into(),
                        UrlWarning::LanHost => "lanHost".into(),
                    },
                    message: w.message().into(),
                })
                .collect()
        })
        .unwrap_or_default();
    Profile {
        id: p.id.clone(),
        name: p.name.clone(),
        kind: p.dialect.clone(),
        base_url: p.base_url.clone(),
        model: p.model.clone(),
        max_output_tokens: extra.get("maxOutputTokens").and_then(Value::as_u64).map(|n| n as u32).unwrap_or(DEFAULT_MAX_OUTPUT),
        anthropic_version: extra.get("anthropicVersion").and_then(Value::as_str).map(str::to_string),
        has_key: p.has_key,
        key_origin: p.key_host.clone(),
        needs_key_reentry: store::needs_key_reentry(p),
        warnings,
        tested_at: extra.get("testedAt").and_then(Value::as_i64),
        test_ok: extra.get("testOk").and_then(Value::as_bool),
    }
}

const BUSY_PERMS: &str = "The assistant is still answering, or an approved suggestion is still running, in this conversation. Wait for it to finish (or stop it), then change the permissions.";

fn check_consent_limits(consent: &Consent) -> R<()> {
    if consent.scopes.len() > 1000 || consent.scopes.iter().any(|s| s.account.len() > 320 || s.folder.len() > 512) {
        return Err("The consent has too many or too long folder entries.".into());
    }
    if consent.calendar.calendars.len() > 200 || consent.calendar.calendars.iter().any(|c| c.id.len() > 128) {
        return Err("The consent has too many or too long calendar entries.".into());
    }
    Ok(())
}

/// The transcript line for a permissions change, or `None` when nothing changed.
fn describe_consent_change(old: &Consent, new: &Consent) -> Option<String> {
    use std::collections::BTreeSet;
    let set = |c: &Consent| -> BTreeSet<(String, String)> { c.scopes.iter().filter(|s| s.allowed).map(|s| (s.account.clone(), s.folder.clone())).collect() };
    let (o, n) = (set(old), set(new));
    let plural = |k: usize, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
    let mut parts: Vec<String> = Vec::new();
    let (added, removed) = (n.difference(&o).count(), o.difference(&n).count());
    if added > 0 {
        parts.push(format!("+{}", plural(added, "folder")));
    }
    if removed > 0 {
        parts.push(format!("-{}", plural(removed, "folder")));
    }
    let cal = |c: &Consent| -> BTreeSet<String> { if c.tools && c.calendar.enabled { c.calendar.calendars.iter().filter(|x| x.allowed).map(|x| x.id.clone()).collect() } else { BTreeSet::new() } };
    let (co, cn) = (cal(old), cal(new));
    if co.is_empty() != cn.is_empty() {
        parts.push(format!("calendar {}", if cn.is_empty() { "off" } else { "on" }));
    } else if co != cn {
        parts.push("calendars changed".into());
    }
    let flag = |name: &str, a: bool, b: bool, parts: &mut Vec<String>| {
        if a != b {
            parts.push(format!("{name} {}", if b { "on" } else { "off" }));
        }
    };
    flag("mail tools", old.tools, new.tools, &mut parts);
    flag("suggestions", old.tools && old.suggest, new.tools && new.suggest, &mut parts);
    if old.redaction != new.redaction {
        parts.push("redaction changed".into());
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("Permissions changed by you: {}", parts.join(", ")))
    }
}

fn conv_to_wire(c: &store::Conversation) -> Conversation {
    Conversation {
        id: c.id,
        title: c.title.clone(),
        profile_id: c.profile_id.clone().unwrap_or_default(),
        endpoint_host: c.endpoint_host.clone(),
        model: c.model.clone(),
        created_at: c.created_at,
        updated_at: c.updated_at,
        tools_enabled: serde_json::from_str::<Consent>(&c.consent_json).map(|c| c.tools).unwrap_or(false),
        suggest_enabled: serde_json::from_str::<Consent>(&c.consent_json).map(|c| c.tools && c.suggest).unwrap_or(false),
        calendar_enabled: serde_json::from_str::<Consent>(&c.consent_json).map(|c| c.tools && c.calendar.enabled && c.calendar.calendars.iter().any(|x| x.allowed)).unwrap_or(false),
    }
}

/// What the conversation's consent lets the tools reach: the folders ticked in the dialog (plus folders created in the conversation via an approved create-folder card, added by `ToolRuntime::new`).
fn tool_setup(host: Arc<dyn MailToolHost>, consent: &Consent, actions: Option<Arc<dyn MailActionHost>>, drafts: Option<Arc<dyn DraftHost>>, calendar: Option<CalSetup>) -> ToolSetup {
    ToolSetup {
        host,
        actions,
        drafts,
        calendar,
        scopes: consent.scopes.iter().filter(|s| s.allowed).map(|s| ToolScope { account: s.account.clone(), folder: s.folder.clone() }).collect(),
        redaction: RedactOpts {
            strip_quotes: consent.redaction.strip_quotes,
            shorten_links: consent.redaction.shorten_links,
            mask_addresses: consent.redaction.mask_addresses,
        },
    }
}

/// Stored rows -> what the UI shows: one entry per user message and ONE assistant entry per turn (the
/// text of every response of the turn joined by a blank line, with the turn's tool calls as chips).
/// Rows that only carry tool calls / tool results are folded away.
fn fold_messages(rows: &[StoredMessage], runs: &[ToolRun]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut pending: Option<Message> = None;
    let flush = |pending: &mut Option<Message>, out: &mut Vec<Message>| {
        if let Some(m) = pending.take() {
            if !m.text.is_empty() || !m.tools.is_empty() {
                out.push(m);
            }
        }
    };
    for r in rows {
        let parts: Vec<Part> = serde_json::from_str(&r.parts_json).unwrap_or_default();
        match r.role.as_str() {
            "user" => {
                if matches!(parts.first(), Some(Part::Text { .. })) {
                    flush(&mut pending, &mut out);
                    let text = aichat::context::display_text(&r.parts_json);
                    if !text.is_empty() {
                        out.push(Message { id: r.id, role: "user".into(), text, created_at: r.created_at, tools: vec![] });
                    }
                }
            }
            "assistant" => {
                let m = pending.get_or_insert_with(|| Message { id: r.id, role: "assistant".into(), text: String::new(), created_at: r.created_at, tools: vec![] });
                let text = aichat::context::display_text(&r.parts_json);
                if !text.is_empty() {
                    if !m.text.is_empty() {
                        m.text.push_str("\n\n");
                    }
                    m.text.push_str(&text);
                }
                for p in &parts {
                    if let Part::ToolCall { id, name, .. } = p {
                        let run = runs.iter().find(|t| &t.call_id == id);
                        m.tools.push(ToolChip {
                            id: id.clone(),
                            name: run.map(|t| t.name.clone()).unwrap_or_else(|| name.chars().take(64).collect()),
                            label: run.map(|t| t.label.clone()).unwrap_or_default(),
                            summary: run.map(|t| t.summary.clone()).unwrap_or_default(),
                            ok: run.map(|t| t.ok).unwrap_or(true),
                        });
                    }
                }
                m.id = r.id;
            }
            "notice" => {
                flush(&mut pending, &mut out);
                let text = aichat::context::display_text(&r.parts_json);
                if !text.is_empty() {
                    out.push(Message { id: r.id, role: "notice".into(), text, created_at: r.created_at, tools: vec![] });
                }
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// OpenAI's own API wants `max_completion_tokens` (its reasoning models reject `max_tokens`);
/// compatible servers keep `max_tokens`.
fn options_for(p: &Profile, base: &BaseUrl, dialect: Dialect) -> ProviderOptions {
    let mut o = ProviderOptions::for_dialect(dialect);
    if let Some(v) = &p.anthropic_version {
        o.anthropic_version = v.clone();
    } else {
        o.anthropic_version = DEFAULT_ANTHROPIC_VERSION.into();
    }
    if dialect == Dialect::OpenAiChat && base.host == "api.openai.com" {
        o.max_tokens_field = MaxTokensField::MaxCompletionTokens;
    }
    o
}

fn runtime_of(prof: &store::Profile, base: &BaseUrl) -> R<ProfileRuntime> {
    let w = profile_to_wire(prof);
    let dialect = parse_kind(&prof.dialect)?;
    Ok(ProfileRuntime {
        profile_id: prof.id.clone(),
        base_url: prof.base_url.clone(),
        model: prof.model.clone(),
        options: options_for(&w, base, dialect),
        max_output_tokens: w.max_output_tokens.max(1),
    })
}

/// Hide models that cannot chat (embeddings, rerankers, speech) so the first listed one is a usable default.
fn is_chat_model(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    !["embed", "minilm", "rerank", "bge-", "whisper", "tts", "moderation"].iter().any(|k| l.contains(k))
}

fn parse_models(body: &[u8]) -> Vec<String> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return vec![] };
    let arr = v.get("data").or_else(|| v.get("models")).and_then(Value::as_array);
    let mut out: Vec<String> = arr
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(Value::as_str))
        .filter(|s| !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control))
        .filter(|s| is_chat_model(s))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out.truncate(500);
    out
}

async fn run_ping(
    client: &http::Client,
    base: &BaseUrl,
    auth: &Auth<'_>,
    opts: &ProviderOptions,
    req: &ChatRequest,
    stop: &tokio::sync::watch::Receiver<bool>,
    limits: &Limits,
) -> Result<(), UserFacingError> {
    let built = provider::build_request(req, opts);
    let hdrs = built.header_refs();
    let mut chunks = http::stream_sse_with(client, base, &built.path, &hdrs, auth, &built.body, stop.clone(), limits).await?;
    let mut sse = SseParser::new();
    let mut parser = StreamParser::new(opts.dialect);
    let bad = |m: String| UserFacingError { kind: ErrorKind::Other, message: m, status: None, retry_after_secs: None };
    let mut done = false;
    while let Some(c) = chunks.next().await {
        for ev in sse.feed(&c?).map_err(|e| bad(e.to_string()))? {
            done |= parser.feed(ev)?.iter().any(|e| matches!(e, StreamEvent::Done(_)));
        }
        if done {
            return Ok(());
        }
    }
    for ev in sse.finish().map_err(|e| bad(e.to_string()))? {
        parser.feed(ev)?;
    }
    parser.finish()?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
