//! Chat turn engine (task A3a). No tools in P0: one user message in, one streamed assistant reply out.
//!
//! Order of operations for [`run_turn`] (the order is the security contract):
//! 1. validate the base URL; refuse a key that is bound to a different origin;
//! 2. build the request within the per-turn budget (see [`crate::context::plan`]) and check the
//!    per-conversation and per-day caps;
//! 3. record the egress row (request body, auth removed) BEFORE anything leaves the machine;
//! 4. open the stream; only then persist the user message (and a new segment, if any);
//! 5. stream events to the [`EventSink`], honouring cancel; persist the assistant reply and usage.
//!
//! Nothing here logs. Keys are only ever handed to `http.rs` as a borrowed header value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use tokio::sync::watch;

use crate::budget::{self, estimate_tokens, Caps};
use crate::context;
use crate::http::{self, Auth, BaseUrl, ErrorKind, Limits, UserFacingError};
use crate::provider::{self, ProviderOptions, StreamParser};
use crate::sse::SseParser;
use crate::store::{NewEgress, Store, ToolRun};
use crate::tools::{self, ToolOutcome, ToolRuntime, ToolSetup, MAX_TOOL_ROUNDS};
use crate::types::{ChatMessage, ChatRequest, Dialect, Part, Role, StopReason, StreamEvent};
use serde_json::Value;

pub const DEFAULT_TITLE: &str = "New conversation";
/// Max accepted user message (chars).
pub const MAX_USER_CHARS: usize = 200_000;
/// Max assistant text accepted from one response (bytes).
const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
}

// ------------------------------------------------------------------------------------------
// Events and errors
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    Text(String),
    Usage { input: u64, output: u64 },
    /// A tool call is about to run. `label` describes it (from the call's arguments).
    ToolStart { id: String, name: String, label: String },
    /// The tool call finished. `summary` is a short app-written outcome such as "12 results".
    ToolEnd { id: String, ok: bool, summary: String },
    /// A suggestion was queued for the user's approval. `title` was written by the app.
    Proposal { id: i64, title: String },
    /// A draft was written and waits for the user to open it in Compose. `title` was written by the app.
    Draft { id: i64, title: String },
    /// A rule was proposed and saved OFF (P4). `title` was written by the app.
    Rule { id: i64, title: String },
    /// `stop`: `end_turn`, `max_tokens`, `refusal`, `content_filter`, `cancelled`, or the provider's own value.
    Done { stop: String },
    Error { message: String, kind: String },
}

/// Receives events as they happen. Must not block for long.
pub trait EventSink: Send + Sync {
    fn send(&self, ev: TurnEvent);
}

/// Collects events (tests).
#[derive(Default)]
pub struct VecSink(pub Mutex<Vec<TurnEvent>>);

impl EventSink for VecSink {
    fn send(&self, ev: TurnEvent) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
    }
}

impl VecSink {
    pub fn events(&self) -> Vec<TurnEvent> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn text(&self) -> String {
        self.events().into_iter().filter_map(|e| if let TurnEvent::Text(t) = e { Some(t) } else { None }).collect()
    }
}

/// A failure whose `message` is safe to show. `kind` is a stable lower-camel tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnError {
    pub kind: String,
    pub message: String,
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for TurnError {}

pub fn kind_tag(k: ErrorKind) -> &'static str {
    match k {
        ErrorKind::InvalidUrl => "invalidUrl",
        ErrorKind::BadRequest => "badRequest",
        ErrorKind::Auth => "auth",
        ErrorKind::Forbidden => "forbidden",
        ErrorKind::NotFound => "notFound",
        ErrorKind::RateLimited => "rateLimited",
        ErrorKind::Server => "server",
        ErrorKind::Redirect => "redirect",
        ErrorKind::Tls => "tls",
        ErrorKind::Timeout => "timeout",
        ErrorKind::Offline => "offline",
        ErrorKind::ConnectionRefused => "connectionRefused",
        ErrorKind::TooLarge => "tooLarge",
        ErrorKind::Cancelled => "cancelled",
        ErrorKind::InvalidKey => "invalidKey",
        ErrorKind::Other => "other",
    }
}

impl From<UserFacingError> for TurnError {
    fn from(e: UserFacingError) -> Self {
        TurnError { kind: kind_tag(e.kind).into(), message: e.message }
    }
}

fn terr(kind: &str, message: impl Into<String>) -> TurnError {
    TurnError { kind: kind.into(), message: message.into() }
}

fn storage_err<E>(_: E) -> TurnError {
    // Underlying text could in principle echo user content; keep it out.
    terr("storage", "MiraMail could not read or write its AI chat database.")
}

// ------------------------------------------------------------------------------------------
// One in-flight turn per conversation
// ------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct InFlight {
    map: Arc<Mutex<HashMap<i64, watch::Sender<bool>>>>,
}

/// Held for the duration of a turn; dropping it frees the conversation.
pub struct TurnTicket {
    conv_id: i64,
    owner: InFlight,
    pub cancel: watch::Receiver<bool>,
}

impl Drop for TurnTicket {
    fn drop(&mut self) {
        self.owner.map.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.conv_id);
    }
}

impl InFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the conversation; fails if a turn is already running on it.
    pub fn begin(&self, conv_id: i64) -> Result<TurnTicket, TurnError> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.contains_key(&conv_id) {
            return Err(terr("busy", "The assistant is still answering in this conversation. Wait for it to finish or stop it first."));
        }
        let (tx, rx) = watch::channel(false);
        m.insert(conv_id, tx);
        Ok(TurnTicket { conv_id, owner: self.clone(), cancel: rx })
    }

    /// Ask the running turn (if any) to stop. Returns whether one was running.
    pub fn cancel(&self, conv_id: i64) -> bool {
        match self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&conv_id) {
            Some(tx) => {
                let _ = tx.send(true);
                true
            }
            None => false,
        }
    }

    pub fn is_running(&self, conv_id: i64) -> bool {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&conv_id)
    }
}

// ------------------------------------------------------------------------------------------
// Context and outcome
// ------------------------------------------------------------------------------------------

/// What the engine needs to know about the endpoint (no secrets).
#[derive(Debug, Clone)]
pub struct ProfileRuntime {
    pub profile_id: String,
    pub base_url: String,
    pub model: String,
    pub options: ProviderOptions,
    pub max_output_tokens: u32,
}

/// Everything for one turn. Deliberately not `Debug` (it carries the key).
pub struct TurnCtx {
    pub store: Arc<Mutex<Store>>,
    pub conv_id: i64,
    pub profile: ProfileRuntime,
    /// The API key, if the endpoint uses one.
    pub api_key: Option<String>,
    /// Origin the key was saved for; the key is refused unless it equals the base URL's origin.
    pub key_origin: Option<String>,
    pub caps: Caps,
    pub limits: Limits,
    pub clock: Clock,
    pub cancel: watch::Receiver<bool>,
    /// Stored with the egress row (the consent's redaction settings, as JSON).
    pub redactions_json: String,
    /// Read-only mail tools for this conversation (`None` = plain chat, identical to P0).
    pub tools: Option<ToolSetup>,
    /// Current time, zone and working hours, put in front of the user's text each turn (never in the system prompt).
    pub env: Option<context::EnvInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    pub stop: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub started_new_segment: bool,
}

fn stop_str(s: &StopReason) -> String {
    match s {
        StopReason::EndTurn => "end_turn".into(),
        StopReason::ToolUse => "tool_use".into(),
        StopReason::MaxTokens => "max_tokens".into(),
        StopReason::Refusal => "refusal".into(),
        StopReason::ContentFilter => "content_filter".into(),
        StopReason::Other(o) => o.clone(),
    }
}

fn lock(s: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------------------
// run_turn
// ------------------------------------------------------------------------------------------

/// Run one user turn. Failures are also delivered to `sink` as an `Error` event; success ends with
/// a `Done` event (`stop: "cancelled"` when the user stopped it).
pub async fn run_turn(ctx: TurnCtx, user_text: &str, sink: &dyn EventSink) -> Result<TurnOutcome, TurnError> {
    match run_inner(&ctx, user_text, sink).await {
        Ok(o) => {
            sink.send(TurnEvent::Done { stop: o.stop.clone() });
            Ok(o)
        }
        Err(e) => {
            sink.send(TurnEvent::Error { message: e.message.clone(), kind: e.kind.clone() });
            Err(e)
        }
    }
}

async fn run_inner(ctx: &TurnCtx, user_text: &str, sink: &dyn EventSink) -> Result<TurnOutcome, TurnError> {
    let user_text = user_text.trim();
    if user_text.is_empty() {
        return Err(terr("badRequest", "Type a message first."));
    }
    if user_text.chars().count() > MAX_USER_CHARS {
        return Err(terr("budget", "That message is too long. Shorten it and try again."));
    }
    let now = (ctx.clock)();
    let base: BaseUrl = http::validate_base_url(&ctx.profile.base_url)?;
    let key: Option<&str> = ctx.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty());
    if key.is_some() && !ctx.key_origin.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&base.origin())) {
        return Err(terr(
            "auth",
            "The address of this AI endpoint changed since the API key was saved. Enter the key again in Settings > AI.",
        ));
    }
    let dialect = ctx.profile.options.dialect;

    // Tools (P1): a per-turn runtime with its own nonce, caps and reference table. `None` = P0 behaviour.
    let mut rt: Option<ToolRuntime> = ctx.tools.as_ref().map(|t| ToolRuntime::new(t.clone(), ctx.store.clone(), ctx.conv_id));
    let tools_on = rt.is_some();
    debug_assert!(rt.as_ref().is_none_or(|r| r.registry().assert_safe()));
    let proposals_on = rt.as_ref().is_some_and(|r| r.registry().has_proposals());
    let drafts_on = rt.as_ref().is_some_and(|r| r.registry().has_drafts());
    let calendar_on = rt.as_ref().is_some_and(|r| r.registry().has_calendar());
    let calendar_propose_on = rt.as_ref().is_some_and(|r| r.registry().has_calendar_propose());
    let mut system = if tools_on { format!("{}\n\n{}", context::SYSTEM_PROMPT, context::TOOLS_ADDENDUM) } else { context::SYSTEM_PROMPT.to_string() };
    if proposals_on {
        system.push_str("\n\n");
        system.push_str(context::PROPOSE_ADDENDUM);
    }
    if drafts_on {
        system.push_str("\n\n");
        system.push_str(context::DRAFT_ADDENDUM);
    }
    if calendar_on {
        system.push_str("\n\n");
        system.push_str(context::CALENDAR_ADDENDUM);
    }
    if calendar_propose_on {
        system.push_str("\n\n");
        system.push_str(context::CALENDAR_PROPOSE_ADDENDUM);
    }
    let specs = rt.as_ref().map(|r| r.registry().specs()).unwrap_or_default();
    let fixed = context::fixed_tokens(tools_on, &specs) + if proposals_on { estimate_tokens(context::PROPOSE_ADDENDUM) + 2 } else { 0 }
        + if drafts_on { estimate_tokens(context::DRAFT_ADDENDUM) + 2 } else { 0 }
        + if calendar_on { estimate_tokens(context::CALENDAR_ADDENDUM) + 2 } else { 0 }
        + if calendar_propose_on { estimate_tokens(context::CALENDAR_PROPOSE_ADDENDUM) + 2 } else { 0 };

    // The environment note travels with the user's message (stored with it, so history replays byte-identically).
    let planned_text: String = match &ctx.env {
        Some(e) => format!("{}{}", context::render_env_note(e), user_text),
        None => user_text.to_string(),
    };

    // 1-2. plan within budgets
    let (plan, conv_title, conv_used, day_used) = {
        let st = lock(&ctx.store);
        let conv = st.get_conversation(ctx.conv_id).map_err(storage_err)?.ok_or_else(|| terr("notFound", "That conversation no longer exists."))?;
        let msgs = st.list_messages(ctx.conv_id).map_err(storage_err)?;
        // Results read from folders the user has since unticked are not replayed to the model.
        let msgs = context::hide_out_of_scope_results(msgs, &st.list_refs(ctx.conv_id).map_err(storage_err)?, ctx.tools.as_ref().map(|t| t.scopes.as_slice()).unwrap_or_default(), ctx.tools.as_ref().and_then(|t| t.calendar.as_ref()).map(|c| c.calendars.as_slice()).unwrap_or_default());
        let seg = st.get_segment(ctx.conv_id).map_err(storage_err)?;
        let plan = context::plan(&msgs, seg.as_ref(), &planned_text, ctx.caps.per_turn, fixed).map_err(|e| terr("budget", e.message))?;
        let conv_used = st.conv_tokens(ctx.conv_id).map_err(storage_err)?.max(0) as u64;
        let day_used = st.tokens_on_day(now).map_err(storage_err)?.max(0) as u64;
        (plan, conv.title, conv_used, day_used)
    };

    // Mail text returned by tools this turn must fit what is left of the per-request budget
    // (the estimator counts ~3 bytes per token; keep half the room for call parts and escaping).
    if let Some(r) = rt.as_mut() {
        let replayed = plan.messages.iter().flat_map(|m| m.parts.iter()).filter(|p| matches!(p, Part::ToolResult { .. })).count();
        r.set_replayed(replayed);
        r.set_char_budget((ctx.caps.per_turn.saturating_sub(plan.est_tokens) as usize).saturating_mul(3) / 2);
    }

    let mut opts = ctx.profile.options.clone();
    opts.dialect = dialect;
    let auth = match (dialect, key) {
        (_, None) => Auth::none(),
        (Dialect::Anthropic, Some(k)) => Auth::x_api_key(k),
        (Dialect::OpenAiChat, Some(k)) => Auth::bearer(k),
    };

    let mut messages = plan.messages.clone();
    let mut client: Option<http::Client> = None;
    let (mut total_in, mut total_out) = (0u64, 0u64);
    let mut emitted_text = false;
    let mut stop = String::from("end_turn");
    let mut used_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for round in 0..=MAX_TOOL_ROUNDS {
        let req = ChatRequest {
            model: ctx.profile.model.clone(),
            system: Some(system.clone()),
            messages: messages.clone(),
            tools: specs.clone(),
            max_output_tokens: ctx.profile.max_output_tokens,
        };
        let built = provider::build_request(&req, &opts);
        let body_text = built.body.to_string();
        let est = if round == 0 && !tools_on { plan.est_tokens } else { estimate_tokens(&body_text) };
        budget::check(&ctx.caps, est, conv_used + total_in + total_out, day_used + total_in + total_out).map_err(|e| {
            terr(
                "budget",
                if round > 0 { format!("{} (reached while reading mail for this question.)", e.message) } else { e.message },
            )
        })?;

        // 3. egress row BEFORE the request leaves (tool results included in the body are listed as items).
        {
            let mut st = lock(&ctx.store);
            st.record_egress(
                &NewEgress {
                    round: round as i64,
                    conv_id: Some(ctx.conv_id),
                    profile_id: Some(ctx.profile.profile_id.clone()),
                    endpoint_host: host_label(&base),
                    model: ctx.profile.model.clone(),
                    request_json: body_text.clone(),
                    bytes: body_text.len() as i64,
                    est_tokens: est as i64,
                    items_json: rt.as_ref().map(|r| r.items_json()).unwrap_or_else(|| "[]".into()),
                    redactions_json: ctx.redactions_json.clone(),
                },
                (ctx.clock)(),
            )
            .map_err(storage_err)?;
        }

        // 4. open the stream
        if client.is_none() {
            client = Some(http::build_client_with(&base, &ctx.limits)?);
        }
        let hdrs = built.header_refs();
        let opened = http::stream_sse_with(client.as_ref().unwrap(), &base, &built.path, &hdrs, &auth, &built.body, ctx.cancel.clone(), &ctx.limits).await;
        let mut chunks = match opened {
            Ok(c) => c,
            Err(e) if e.kind == ErrorKind::Cancelled => {
                return Ok(TurnOutcome { stop: "cancelled".into(), input_tokens: total_in, output_tokens: total_out, started_new_segment: plan.new_segment.is_some() && round > 0 });
            }
            Err(e) => return Err(e.into()),
        };

        if round == 0 {
            // The endpoint accepted the request: persist the user message (and the new segment).
            let mut st = lock(&ctx.store);
            let parts = serde_json::to_string(&[Part::text(planned_text.as_str())]).map_err(storage_err)?;
            st.append_message(ctx.conv_id, "user", &parts, now).map_err(storage_err)?;
            if let Some(seg) = &plan.new_segment {
                st.set_segment(ctx.conv_id, seg.start_seq, &seg.summary).map_err(storage_err)?;
            }
            if conv_title.is_empty() || conv_title == DEFAULT_TITLE {
                let _ = st.rename_conversation(ctx.conv_id, &title_from(user_text));
            }
        }

        // 5. stream
        let mut sse = SseParser::new();
        let mut parser = StreamParser::new(dialect);
        let mut acc = Accum { sep_before_text: emitted_text, ..Accum::default() };
        let mut failure: Option<TurnError> = None;
        let mut cancelled = false;

        'outer: while let Some(chunk) = chunks.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) if e.kind == ErrorKind::Cancelled => {
                    cancelled = true;
                    break;
                }
                Err(e) => {
                    failure = Some(e.into());
                    break;
                }
            };
            let events = match sse.feed(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    failure = Some(terr("other", e.to_string()));
                    break;
                }
            };
            for ev in events {
                match parser.feed(ev) {
                    Ok(out) => acc.apply(out, sink),
                    Err(e) => {
                        failure = Some(e.into());
                        break 'outer;
                    }
                }
                if acc.text.len() > MAX_REPLY_BYTES {
                    failure = Some(terr("tooLarge", "The reply grew unusually large and was stopped."));
                    break 'outer;
                }
            }
        }
        drop(chunks);
        if failure.is_none() && !cancelled {
            // End of body: flush the parsers (this is where a cut-off stream becomes an error).
            let tail = sse.finish().map_err(|e| terr("other", e.to_string())).and_then(|evs| {
                let mut all = Vec::new();
                for ev in evs {
                    all.extend(parser.feed(ev).map_err(TurnError::from)?);
                }
                all.extend(parser.finish().map_err(TurnError::from)?);
                Ok(all)
            });
            match tail {
                Ok(out) => acc.apply(out, sink),
                Err(e) => failure = Some(e),
            }
        }
        emitted_text |= acc.text_started;

        // usage of this request
        let in_tok = if acc.usage_in > 0 { acc.usage_in } else { est };
        let out_tok = if acc.usage_out > 0 { acc.usage_out } else { estimate_tokens(&acc.text) };
        total_in += in_tok;
        total_out += out_tok;
        {
            let mut st = lock(&ctx.store);
            let _ = st.add_usage((ctx.clock)(), &ctx.profile.profile_id, &ctx.profile.model, in_tok as i64, out_tok as i64);
            let _ = st.add_conv_tokens(ctx.conv_id, (in_tok + out_tok) as i64);
        }

        // Tool calls this response asked for (only when tools are on and the response is intact).
        let wants_tools = tools_on && failure.is_none() && !cancelled && !acc.calls.is_empty();
        let over_limit = wants_tools && round >= MAX_TOOL_ROUNDS;

        if wants_tools && !over_limit {
            // 6a. run the calls; persist the assistant message and the results together afterwards so the
            // stored history never contains a tool call without its result.
            let rt_ref = rt.as_mut().expect("tools_on implies runtime");
            let mut results: Vec<Part> = Vec::new();
            let mut runs: Vec<ToolRun> = Vec::new();
            let mut call_parts: Vec<Part> = Vec::new();
            let last_round = round + 1 >= MAX_TOOL_ROUNDS;
            let mut calls = std::mem::take(&mut acc.calls);
            calls.truncate(MAX_CALLS_PER_RESPONSE);
            let ncalls = calls.len();
            for (i, mut call) in calls.into_iter().enumerate() {
                if *ctx.cancel.borrow() {
                    cancelled = true;
                    break;
                }
                if !valid_call_id(&call.id) || !used_call_ids.insert(call.id.clone()) {
                    call.id = format!("call_{round}_{i}");
                    used_call_ids.insert(call.id.clone());
                }
                let id = call.id.clone();
                let parsed: Result<Value, ()> = if call.args.trim().is_empty() { Ok(Value::Object(Default::default())) } else { serde_json::from_str(&call.args).map_err(|_| ()) };
                let args_for_part = parsed.clone().unwrap_or_else(|_| Value::Object(Default::default()));
                let label = tools::label_for(&call.name, &args_for_part);
                sink.send(TurnEvent::ToolStart { id: id.clone(), name: clean_name(&call.name), label: label.clone() });
                let mut cancel_rx = ctx.cancel.clone();
                let outcome = match &parsed {
                    Err(()) => ToolOutcome { content: "The arguments were not valid JSON.".into(), ok: false, label: label.clone(), summary: "invalid call".into() },
                    Ok(v) => {
                        tokio::select! {
                            o = rt_ref.execute(&call.name, v) => o,
                            _ = cancel_rx.wait_for(|c| *c) => { cancelled = true; break; }
                        }
                    }
                };
                sink.send(TurnEvent::ToolEnd { id: id.clone(), ok: outcome.ok, summary: outcome.summary.clone() });
                for (pid, title) in rt_ref.take_new_proposals() {
                    sink.send(TurnEvent::Proposal { id: pid, title });
                }
                for (did, title) in rt_ref.take_new_drafts() {
                    sink.send(TurnEvent::Draft { id: did, title });
                }
                for (rid, title) in rt_ref.take_new_rules() {
                    sink.send(TurnEvent::Rule { id: rid, title });
                }
                let mut content = outcome.content;
                if last_round && i + 1 == ncalls {
                    content.push_str("\n[MiraMail: the tool-round limit for this question is reached. Do not call more tools; answer now with what you have.]");
                }
                call_parts.push(Part::ToolCall { id: id.clone(), name: call.name.clone(), args: args_for_part });
                results.push(Part::ToolResult { call_id: id.clone(), content, is_error: !outcome.ok });
                runs.push(ToolRun { call_id: id, name: clean_name(&call.name), label: outcome.label, summary: outcome.summary, ok: outcome.ok });
            }
            if !cancelled {
                let mut asst: Vec<Part> = acc.opaque.clone();
                if !acc.text.is_empty() {
                    asst.push(Part::text(acc.text.clone()));
                }
                asst.extend(call_parts);
                {
                    let mut st = lock(&ctx.store);
                    let t = (ctx.clock)();
                    st.append_message(ctx.conv_id, "assistant", &serde_json::to_string(&asst).map_err(storage_err)?, t).map_err(storage_err)?;
                    st.append_message(ctx.conv_id, "user", &serde_json::to_string(&results).map_err(storage_err)?, t).map_err(storage_err)?;
                    for r in &runs {
                        let _ = st.put_tool_run(ctx.conv_id, r);
                    }
                }
                messages.push(ChatMessage { role: Role::Assistant, parts: asst });
                messages.push(ChatMessage { role: Role::User, parts: results });
                continue;
            }
            // cancelled while running tools: fall through and keep only the text of this response.
        }

        // 6b. final persistence of this response (text only; unanswered tool calls are never stored)
        {
            let mut st = lock(&ctx.store);
            let mut parts: Vec<Part> = Vec::new();
            parts.extend(acc.opaque.iter().cloned());
            if !acc.text.is_empty() {
                parts.push(Part::text(acc.text.clone()));
            }
            if !parts.is_empty() {
                let pj = serde_json::to_string(&parts).map_err(storage_err)?;
                st.append_message(ctx.conv_id, "assistant", &pj, (ctx.clock)()).map_err(storage_err)?;
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
        if !acc.usage_reported {
            sink.send(TurnEvent::Usage { input: in_tok, output: out_tok });
        }
        stop = if cancelled {
            "cancelled".to_string()
        } else if over_limit {
            "tool_limit".to_string()
        } else {
            acc.stop.clone().unwrap_or_else(|| "end_turn".into())
        };
        break;
    }
    Ok(TurnOutcome { stop, input_tokens: total_in, output_tokens: total_out, started_new_segment: plan.new_segment.is_some() })
}

/// Tool names come from the model: keep them short and printable before they reach the UI or the DB.
/// Most tool calls honoured from one model response; the rest are dropped.
const MAX_CALLS_PER_RESPONSE: usize = 25;

/// A call id we are willing to store and echo back: short, plain ASCII, no spaces or control chars.
fn valid_call_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn clean_name(n: &str) -> String {
    n.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(64).collect()
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    args: String,
}

#[derive(Default)]
struct Accum {
    text: String,
    opaque: Vec<Part>,
    calls: Vec<PendingCall>,
    usage_in: u64,
    usage_out: u64,
    usage_reported: bool,
    stop: Option<String>,
    /// Earlier rounds already showed text: start this round's text with a paragraph break.
    sep_before_text: bool,
    text_started: bool,
}

/// Max bytes of tool-call arguments accepted per call.
const MAX_CALL_ARGS_BYTES: usize = 64 * 1024;

impl Accum {
    fn apply(&mut self, events: Vec<StreamEvent>, sink: &dyn EventSink) {
        for ev in events {
            match ev {
                StreamEvent::TextDelta(t) => {
                    if !t.is_empty() {
                        if self.sep_before_text && !self.text_started {
                            sink.send(TurnEvent::Text("\n\n".into()));
                        }
                        self.text_started = true;
                        self.text.push_str(&t);
                        sink.send(TurnEvent::Text(t));
                    }
                }
                StreamEvent::Usage { input, output, .. } => {
                    self.usage_in = input;
                    self.usage_out = output;
                    self.usage_reported = true;
                    sink.send(TurnEvent::Usage { input, output });
                }
                StreamEvent::Opaque { dialect, raw } => self.opaque.push(Part::Opaque { dialect, raw }),
                StreamEvent::Done(s) => self.stop = Some(stop_str(&s)),
                // Calls are collected; whether they run depends on the turn having tools (P0 ignores them).
                StreamEvent::ToolCallStart { id, name, .. } => self.calls.push(PendingCall { id, name, args: String::new() }),
                StreamEvent::ToolCallArgsDelta { idx, frag } => {
                    if let Some(c) = self.calls.get_mut(idx) {
                        if c.args.len() + frag.len() <= MAX_CALL_ARGS_BYTES {
                            c.args.push_str(&frag);
                        }
                    }
                }
                StreamEvent::ToolCallEnd { .. } => {}
            }
        }
    }
}

/// `host` or `host:port` (port omitted when it is the scheme's default).
pub fn host_label(b: &BaseUrl) -> String {
    let default = match b.scheme {
        http::Scheme::Http => 80,
        http::Scheme::Https => 443,
    };
    let h = if b.host.contains(':') { format!("[{}]", b.host) } else { b.host.clone() };
    if b.port == default {
        h
    } else {
        format!("{h}:{}", b.port)
    }
}

fn title_from(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    let mut t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        t.push('…');
    }
    if t.is_empty() {
        DEFAULT_TITLE.to_string()
    } else {
        t
    }
}

/// Convenience for the glue and tests: the stored messages as display `(role, text)` pairs.
pub fn display_messages(rows: &[crate::store::StoredMessage]) -> Vec<(i64, String, String, i64)> {
    rows.iter().map(|r| (r.id, r.role.clone(), context::display_text(&r.parts_json), r.created_at)).collect()
}
