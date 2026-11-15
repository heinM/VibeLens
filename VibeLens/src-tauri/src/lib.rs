//! VibeLens desktop shell: wires `vibelens-core` services into Tauri and exposes IPC commands.

mod commands;
mod lifecycle;
mod state;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Debug lines carry connection metadata, so release builds only emit them via RUST_LOG.
    let default_filter = if cfg!(debug_assertions) {
        "info,vibelens=debug,vibelens_core=debug"
    } else {
        "warn,vibelens=info,vibelens_core=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(lifecycle::Lifecycle::default())
        .on_window_event(lifecycle::on_window_event)
        .on_menu_event(lifecycle::on_menu_event)
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_menu(lifecycle::menu(app.handle())?)?;
            let config_dir = app.path().app_config_dir()?;
            let workspace_dir = dirs::document_dir()
                .map(|documents| documents.join("VibeLens"))
                .unwrap_or_else(|| config_dir.join("workspace"));

            if let Ok(res) = app.path().resource_dir() {
                vibelens_core::tls::set_rds_bundle_path(
                    res.join("resources").join("rds-global-bundle.pem"),
                );
            }

            let state =
                state::AppState::build(config_dir, workspace_dir).map_err(|e| e.to_string())?;
            let connections = state.connections.clone();
            app.manage(state);
            tauri::async_runtime::spawn(async move { connections.start_idle_reaper() });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_info,
            commands::app::sql_classify,
            lifecycle::app_set_unsaved,
            commands::profiles::list_profiles,
            commands::profiles::save_profile,
            commands::profiles::delete_profile,
            commands::profiles::test_connection,
            commands::profiles::profile_secret_status,
            commands::connections::connect,
            commands::connections::disconnect,
            commands::connections::connection_status,
            commands::pg::pg_list_databases,
            commands::pg::pg_list_schemas,
            commands::pg::pg_list_objects,
            commands::pg::pg_describe_table,
            commands::pg::pg_execute,
            commands::pg::pg_cancel,
            commands::pg::pg_schema_metadata,
            commands::files::export_file,
            commands::import::import_scan,
            commands::import::import_choose_file,
            commands::import::import_text,
            commands::import::import_save,
            commands::iceberg::ice_list_namespaces,
            commands::iceberg::ice_list_tables,
            commands::iceberg::ice_describe_table,
            commands::iceberg::ice_schema_metadata,
            commands::iceberg::ice_execute,
            commands::iceberg::ice_expire_snapshots,
            commands::valkey::vk_server_info,
            commands::valkey::vk_list_dbs,
            commands::valkey::vk_scan,
            commands::valkey::vk_get_value,
            commands::valkey::vk_command,
            commands::aws::aws_list_profiles,
            commands::aws::aws_sso_start,
            commands::aws::aws_sso_complete,
            commands::aws::aws_sso_sessions,
            commands::aws::aws_sso_list_accounts,
            commands::aws::aws_sso_list_roles,
            commands::agent::agent_list_providers,
            commands::agent::agent_save_provider,
            commands::agent::agent_delete_provider,
            commands::agent::agent_start_turn,
            commands::agent::agent_resolve_approval,
            commands::agent::agent_cancel,
            commands::agent::agent_reset,
            commands::workspace::ws_list,
            commands::workspace::ws_read,
            commands::workspace::ws_write,
        ])
        .run(tauri::generate_context!())
        .expect("error while running VibeLens");
}
