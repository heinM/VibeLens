use secrecy::SecretString;
use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::mpsc;
use vibelens_core::model::*;

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn agent_list_providers(state: State<'_, AppState>) -> CmdResult<Vec<ProviderSummary>> {
    state.agent.list_providers().await.map_err(err)
}

/// `api_key`: `None` keeps the stored key, `Some("")` deletes it.
#[tauri::command]
pub async fn agent_save_provider(
    state: State<'_, AppState>,
    config: ProviderConfig,
    api_key: Option<String>,
) -> CmdResult<()> {
    state
        .agent
        .save_provider(config, api_key.map(SecretString::from))
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn agent_delete_provider(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.agent.delete_provider(&id).await.map_err(err)
}

#[tauri::command]
pub async fn agent_start_turn(
    state: State<'_, AppState>,
    session_id: String,
    provider_id: String,
    prompt: String,
    context: AgentContext,
    on_event: Channel<AgentEvent>,
) -> CmdResult<String> {
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if on_event.send(ev).is_err() {
                break;
            }
        }
    });
    state
        .agent
        .start_turn(session_id, provider_id, prompt, context, tx)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn agent_resolve_approval(
    state: State<'_, AppState>,
    session_id: String,
    call_id: String,
    decision: ApprovalDecision,
) -> CmdResult<()> {
    state
        .agent
        .resolve_approval(&session_id, &call_id, decision)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn agent_cancel(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    state.agent.cancel(&session_id).await.map_err(err)
}

#[tauri::command]
pub async fn agent_reset(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    state.agent.reset(&session_id).await.map_err(err)
}
