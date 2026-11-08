//! Tauri commands for AI chat. Thin: all logic is in [`super::AiState`]. Errors reject with a
//! plain user-facing string. Contract: docs/AI_API.md.

use super::wire::*;
use super::AiState;
use aichat::engine::{EventSink, TurnEvent};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};

type R<T> = Result<T, String>;

/// Forwards engine events to the webview.
struct ChannelSink(Channel<AiEvent>);

impl EventSink for ChannelSink {
    fn send(&self, ev: TurnEvent) {
        let _ = self.0.send(map_event(ev));
    }
}

pub fn map_event(ev: TurnEvent) -> AiEvent {
    match ev {
        TurnEvent::Text(delta) => AiEvent::Text { delta },
        TurnEvent::Usage { input, output } => AiEvent::Usage { input, output },
        TurnEvent::ToolStart { id, name, label } => AiEvent::ToolStart { id, name, label },
        TurnEvent::ToolEnd { id, ok, summary } => AiEvent::ToolEnd { id, ok, summary },
        TurnEvent::Proposal { id, title } => AiEvent::Proposal { id: id.to_string(), summary: title },
        TurnEvent::Draft { id, title } => AiEvent::Draft { id: id.to_string(), summary: title },
        TurnEvent::Rule { id, title } => AiEvent::Rule { id: id.to_string(), summary: title },
        TurnEvent::Done { stop } => AiEvent::Done { stop },
        TurnEvent::Error { message, kind } => AiEvent::Error { message, kind },
    }
}

#[tauri::command]
pub async fn ai_list_profiles(state: State<'_, AiState>) -> R<Vec<Profile>> {
    state.list_profiles()
}

#[tauri::command]
pub async fn ai_save_profile(state: State<'_, AiState>, profile: Profile, api_key: Option<String>) -> R<Profile> {
    state.save_profile(profile, api_key).await
}

#[tauri::command]
pub async fn ai_delete_profile(state: State<'_, AiState>, id: String) -> R<()> {
    state.delete_profile(&id).await
}

#[tauri::command]
pub async fn ai_test_profile(state: State<'_, AiState>, profile: Profile, api_key: Option<String>) -> R<TestResult> {
    Ok(state.test_profile(profile, api_key).await)
}

#[tauri::command]
pub async fn ai_list_models(state: State<'_, AiState>, profile: Profile, api_key: Option<String>) -> R<TestResult> {
    Ok(state.list_models(profile, api_key).await)
}

/// "Check tone" in Compose: ONE request carrying only the draft text; nothing is stored. Contract: docs/AI_API.md.
#[tauri::command]
pub async fn ai_check_tone(state: State<'_, AiState>, profile_id: String, text: String) -> R<aichat::tone::ToneReport> {
    state.check_tone(profile_id, text).await
}

#[tauri::command]
pub async fn ai_get_settings(state: State<'_, AiState>) -> R<Settings> {
    state.get_settings()
}

#[tauri::command]
pub async fn ai_set_settings(state: State<'_, AiState>, settings: Settings) -> R<Settings> {
    state.set_settings(settings)
}

#[tauri::command]
pub async fn ai_new_conversation(state: State<'_, AiState>, profile_id: String, consent: Consent) -> R<Conversation> {
    state.new_conversation(&profile_id, consent)
}

/// "Edit permissions": the ONLY way a conversation's consent changes, and only from the dialog's Save button.
#[tauri::command]
pub async fn ai_update_consent(state: State<'_, AiState>, conv_id: i64, consent: Consent) -> R<ConversationWithMessages> {
    state.update_consent(conv_id, consent).await
}

#[tauri::command]
pub async fn ai_list_conversations(state: State<'_, AiState>) -> R<Vec<Conversation>> {
    state.list_conversations()
}

#[tauri::command]
pub async fn ai_get_conversation(state: State<'_, AiState>, id: i64) -> R<ConversationWithMessages> {
    state.get_conversation(id)
}

#[tauri::command]
pub async fn ai_delete_conversation(state: State<'_, AiState>, id: i64) -> R<()> {
    state.delete_conversation(id)
}

#[tauri::command]
pub async fn ai_send(state: State<'_, AiState>, conv_id: i64, text: String, on_event: Channel<AiEvent>) -> R<()> {
    state.send(conv_id, text, &ChannelSink(on_event)).await
}

#[tauri::command]
pub async fn ai_cancel(state: State<'_, AiState>, conv_id: i64) -> R<()> {
    state.cancel(conv_id);
    Ok(())
}

#[tauri::command]
pub async fn ai_egress(state: State<'_, AiState>, conv_id: i64, limit: u32) -> R<Vec<EgressRow>> {
    state.egress(conv_id, limit)
}

#[tauri::command]
pub async fn ai_purge_now(state: State<'_, AiState>) -> R<PurgeReport> {
    state.purge_now()
}

// ---- proposals (P2) --------------------------------------------------------------------------

/// Tell the webview a proposal changed (`ai://proposal`).
fn emit_proposal(app: &AppHandle, state: &AiState, id: i64) {
    if let Some(ev) = state.proposal_event(id) {
        let _ = app.emit("ai://proposal", ev);
    }
}

/// The existing mail-changed mechanism: `mail://synced` makes the UI refresh unread counts and the open
/// folder from the (already updated) local cache.
fn emit_mail_changed(app: &AppHandle, affected: &[(String, String)]) {
    let mut by_account: std::collections::BTreeMap<&str, Vec<String>> = std::collections::BTreeMap::new();
    for (a, f) in affected {
        by_account.entry(a.as_str()).or_default().push(f.clone());
    }
    for (account, folders) in by_account {
        let _ = app.emit("mail://synced", crate::wire::SyncedEvent { account: account.to_string(), folders });
    }
}

#[tauri::command]
pub async fn ai_list_proposals(state: State<'_, AiState>, conv_id: Option<i64>, limit: Option<u32>) -> R<Vec<ProposalSummary>> {
    state.list_proposals(conv_id, limit.unwrap_or(100))
}

#[tauri::command]
pub async fn ai_get_proposal(state: State<'_, AiState>, id: i64) -> R<ProposalDetail> {
    state.get_proposal(id)
}

/// Approve and run. `selected` = `idx` of the preview rows the user ticked; nothing else runs.
#[tauri::command]
pub async fn ai_approve_proposal(app: AppHandle, state: State<'_, AiState>, id: i64, selected: Option<Vec<i64>>) -> R<ApproveResult> {
    let st: &AiState = &state;
    let app2 = app.clone();
    let progress = move |pid: i64| emit_proposal(&app2, st, pid);
    let (res, affected) = st.approve_proposal(id, selected.unwrap_or_default(), &progress).await?;
    emit_mail_changed(&app, &affected);
    Ok(res)
}

#[tauri::command]
pub async fn ai_reject_proposal(app: AppHandle, state: State<'_, AiState>, id: i64) -> R<ProposalDetail> {
    let d = state.reject_proposal(id)?;
    emit_proposal(&app, &state, id);
    Ok(d)
}

#[tauri::command]
pub async fn ai_undo_proposal(app: AppHandle, state: State<'_, AiState>, id: i64) -> R<UndoResult> {
    let st: &AiState = &state;
    let app2 = app.clone();
    let progress = move |pid: i64| emit_proposal(&app2, st, pid);
    let (res, affected) = st.undo_proposal(id, &progress).await?;
    emit_mail_changed(&app, &affected);
    Ok(res)
}

// ---- drafts (P3) ------------------------------------------------------------------------------
// None of these can send mail: opening a draft only returns the text and recipients for a Compose pane,
// where the user presses Send.

fn emit_draft(app: &AppHandle, ev: DraftEvent) {
    let _ = app.emit("ai://draft", ev);
}

#[tauri::command]
pub async fn ai_list_drafts(state: State<'_, AiState>, conv_id: Option<i64>, limit: Option<u32>) -> R<Vec<DraftSummary>> {
    state.list_drafts(conv_id, limit.unwrap_or(100))
}

#[tauri::command]
pub async fn ai_get_draft(state: State<'_, AiState>, id: i64) -> R<DraftSummary> {
    state.get_draft(id)
}

/// Mark the draft opened and return what Compose is prefilled with. The webview creates the pane.
#[tauri::command]
pub async fn ai_open_draft(app: AppHandle, state: State<'_, AiState>, id: i64) -> R<DraftPrefill> {
    let (prefill, ev) = state.open_draft(id)?;
    emit_draft(&app, ev);
    Ok(prefill)
}

#[tauri::command]
pub async fn ai_dismiss_draft(app: AppHandle, state: State<'_, AiState>, id: i64) -> R<DraftSummary> {
    let (d, ev) = state.dismiss_draft(id)?;
    emit_draft(&app, ev);
    Ok(d)
}
