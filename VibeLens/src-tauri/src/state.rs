use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use vibelens_core::Result;
use vibelens_core::agent::AgentService;
use vibelens_core::aws::{AppCredentialSource, AwsService};
use vibelens_core::connections::{AppToolBackend, ConnectionManager};
use vibelens_core::ports::{ProfileStore, SecretVault};
use vibelens_core::profiles::JsonProfileStore;
use vibelens_core::tunnel::SshTunnelProvider;
use vibelens_core::vault::{MemoryVault, native_vault};
use vibelens_core::workspace::WorkspaceFs;

pub struct AppState {
    pub profiles: Arc<dyn ProfileStore>,
    pub vault: Arc<dyn SecretVault>,
    pub vault_backend: &'static str,
    pub aws: Arc<AwsService>,
    pub connections: Arc<ConnectionManager>,
    pub workspace: Arc<WorkspaceFs>,
    pub agent: Arc<AgentService>,
    pub running_queries: Mutex<HashMap<String, CancellationToken>>,
    pub config_dir: PathBuf,
}

impl AppState {
    pub fn build(config_dir: PathBuf, workspace_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&config_dir)?;

        let (vault, vault_backend): (Arc<dyn SecretVault>, &'static str) = match native_vault() {
            Ok(v) => (v, "keychain"),
            Err(e) => {
                tracing::warn!(
                    "OS keychain unavailable ({e}); secrets will NOT persist across restarts"
                );
                (Arc::new(MemoryVault::new()), "memory")
            }
        };

        let profiles: Arc<dyn ProfileStore> = Arc::new(JsonProfileStore::new(config_dir.clone())?);
        let aws = AwsService::new();
        let creds = AppCredentialSource::new(aws.clone(), vault.clone());

        // The tunnel provider reads SSH secrets and never writes a profile: a host key is pinned
        // only by the `trust_host_key` command, which the user triggers from a dialog.
        let tunnels = SshTunnelProvider::new(vault.clone());

        let connections = ConnectionManager::new(profiles.clone(), creds, tunnels);
        connections.set_iceberg_deps(vibelens_core::iceberg::IcebergDeps {
            aws: aws.clone(),
            vault: vault.clone(),
        });
        let workspace = Arc::new(WorkspaceFs::new(workspace_dir)?);
        let tool_backend = AppToolBackend::new(connections.clone(), workspace.clone());
        let agent = AgentService::new(vault.clone(), tool_backend, config_dir.clone());

        Ok(Self {
            profiles,
            vault,
            vault_backend,
            aws,
            connections,
            workspace,
            agent,
            running_queries: Mutex::new(HashMap::new()),
            config_dir,
        })
    }
}
