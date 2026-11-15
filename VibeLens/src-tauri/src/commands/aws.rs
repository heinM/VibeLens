use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use vibelens_core::model::*;

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn aws_list_profiles(state: State<'_, AppState>) -> CmdResult<Vec<AwsProfileSummary>> {
    state.aws.list_profiles().await.map_err(err)
}

/// Starts Identity Center device authorization and opens the system browser.
#[tauri::command]
pub async fn aws_sso_start(
    app: AppHandle,
    state: State<'_, AppState>,
    start_url: String,
    region: String,
) -> CmdResult<DeviceLoginStart> {
    let start = state
        .aws
        .sso_start(&start_url, &region)
        .await
        .map_err(err)?;
    let url = start
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| start.verification_uri.clone());
    if let Err(e) = app.opener().open_url(url, None::<&str>) {
        tracing::warn!("could not open browser for SSO: {e}");
    }
    Ok(start)
}

#[tauri::command]
pub async fn aws_sso_complete(
    state: State<'_, AppState>,
    login_id: String,
) -> CmdResult<SsoSessionInfo> {
    state.aws.sso_complete(&login_id).await.map_err(err)
}

#[tauri::command]
pub async fn aws_sso_sessions(state: State<'_, AppState>) -> CmdResult<Vec<SsoSessionInfo>> {
    state.aws.sso_sessions().await.map_err(err)
}

#[tauri::command]
pub async fn aws_sso_list_accounts(
    state: State<'_, AppState>,
    start_url: String,
) -> CmdResult<Vec<SsoAccount>> {
    state.aws.sso_list_accounts(&start_url).await.map_err(err)
}

#[tauri::command]
pub async fn aws_sso_list_roles(
    state: State<'_, AppState>,
    start_url: String,
    account_id: String,
) -> CmdResult<Vec<SsoRole>> {
    state
        .aws
        .sso_list_roles(&start_url, &account_id)
        .await
        .map_err(err)
}
