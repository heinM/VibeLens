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
    pub running_queries: RunningQueries,
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
            running_queries: RunningQueries::default(),
            config_dir,
        })
    }
}

/// The statements that are running, by query id, so that one can be cancelled (`pg_cancel`,
/// for every engine).
#[derive(Clone, Default)]
pub struct RunningQueries(Arc<Mutex<HashMap<String, CancellationToken>>>);

/// One running statement. It is registered for as long as this value lives: the task that
/// executes the statement owns it, so the entry goes when that task ends, however it ends.
/// (Entries used to be removed only by a cancel, and every statement that ran to its end
/// stayed in the map for the life of the app.)
pub struct RunningQuery {
    pub id: String,
    pub cancel: CancellationToken,
    registry: RunningQueries,
}

impl Drop for RunningQuery {
    fn drop(&mut self) {
        self.registry.entries().remove(&self.id);
    }
}

impl RunningQueries {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a new run under a fresh id.
    pub fn start(&self) -> RunningQuery {
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        self.entries().insert(id.clone(), cancel.clone());
        RunningQuery {
            id,
            cancel,
            registry: self.clone(),
        }
    }

    /// Cancels the run with this id and forgets it. `false` if there is none.
    pub fn cancel(&self, id: &str) -> bool {
        match self.entries().remove(id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// How many runs are registered.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries().len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::RunningQueries;

    /// Entries used to be removed only by a cancel, so every statement that ran to its end
    /// stayed in the map for the life of the app.
    #[test]
    fn a_run_is_forgotten_when_it_ends() {
        let running = RunningQueries::default();
        let run = running.start();
        let id = run.id.clone();
        assert_eq!(running.len(), 1);
        drop(run);
        assert_eq!(running.len(), 0, "the run ended; its entry must go");
        assert!(!running.cancel(&id), "nothing left to cancel");
    }

    #[test]
    fn many_finished_runs_leave_nothing_behind() {
        let running = RunningQueries::default();
        for _ in 0..1000 {
            let run = running.start();
            assert!(!run.cancel.is_cancelled());
        }
        assert!(running.is_empty(), "{} entries left", running.len());
    }

    #[test]
    fn cancelling_fires_the_token_and_forgets_the_run() {
        let running = RunningQueries::default();
        let run = running.start();
        let other = running.start();
        assert!(running.cancel(&run.id));
        assert!(run.cancel.is_cancelled());
        assert!(!other.cancel.is_cancelled(), "only the named run");
        assert!(!running.cancel(&run.id), "a second cancel finds nothing");
        assert!(!running.cancel("no-such-id"));
        // The cancelled run's task still ends later; that must not disturb anything.
        drop(run);
        assert_eq!(running.len(), 1);
        drop(other);
        assert!(running.is_empty());
    }

    /// The run's guard lives in the task that executes the statement: when the task ends,
    /// however it ends, the entry goes.
    #[test]
    fn a_run_is_forgotten_when_its_task_panics() {
        let running = RunningQueries::default();
        let run = running.start();
        let task = std::thread::spawn(move || {
            let _run = run;
            panic!("the statement's task died");
        });
        assert!(task.join().is_err());
        assert!(running.is_empty());
    }
}
