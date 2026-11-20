//! SSH local port-forwarding (OWNER: connections agent).
//!
//! `russh` (pure-Rust, async) instead of `ssh2` (blocking libssh2 + vendored OpenSSL).
//!
//! **Host keys: pinned, never trusted silently.** The profile's `known_host_fingerprint` must
//! equal `SHA256:<base64-nopad>` of the key the server presents, otherwise the connection fails
//! with `CoreError::Tunnel("host key mismatch …")`. With no pin the connection stops at the same
//! point with [`CoreError::UnknownHostKey`], which carries the fingerprint the server presented
//! so the user can check it and pin it (`profiles::trust_host_key`). Either way the key
//! exchange is rejected, so the server gets no authentication request: no user name, no key
//! offer, no password. This module never writes a pin.
//!
//! **Auth:** ssh-agent (`SSH_AUTH_SOCK` on unix, the OpenSSH named pipe on Windows), a private key
//! file (optional passphrase from the vault) or a password from the vault (with a
//! keyboard-interactive fallback for PAM-only servers).
//!
//! **Forwarding:** a listener on `127.0.0.1:0`; every accepted socket opens its own `direct-tcpip`
//! channel to `target_host:target_port` and is piped both ways. Everything stops when the returned
//! handle's `CancellationToken` fires (it fires on drop), or when the SSH session dies.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use secrecy::ExposeSecret;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::error::{CoreError, Result};
use crate::model::{PresentedHostKey, SshAuth, TunnelConfig, vault_keys};
use crate::ports::{SecretVault, TunnelHandle, TunnelProvider};

/// Time allowed for TCP connect + SSH key exchange.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Time allowed for the whole authentication exchange.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
/// Ping the server after this much silence; with `KEEPALIVE_MAX` misses the session is dropped.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_MAX: usize = 3;
/// How often the accept loop checks whether the SSH session has died underneath it.
const LIVENESS_POLL: Duration = Duration::from_secs(1);
/// Windows OpenSSH agent named pipe.
#[cfg(windows)]
const WINDOWS_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

pub struct SshTunnelProvider {
    vault: Arc<dyn SecretVault>,
}

impl SshTunnelProvider {
    pub fn new(vault: Arc<dyn SecretVault>) -> Arc<Self> {
        Arc::new(Self { vault })
    }
}

/// `SHA256:<base64 without padding>` of the server's public key (for certificates: of the
/// certified key, which is what OpenSSH users see in `ssh-keygen -lf`).
fn fingerprint_of(key: &PublicKeyOrCertificate) -> String {
    match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => {
            key.fingerprint(HashAlg::Sha256).to_string()
        }
        PublicKeyOrCertificate::Certificate(cert) => {
            cert.public_key().fingerprint(HashAlg::Sha256).to_string()
        }
    }
}

/// Client-side event handler: accepts the pinned host key and no other, and records what the
/// server presented.
struct HostKeyHandler {
    pinned: Option<String>,
    /// Fingerprint the server presented, readable after the handshake (also after a rejection).
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let fp = fingerprint_of(server_public_key);
        // No pin is a refusal too: the key exchange fails here, which ends the connection
        // before the authentication protocol starts. `open` turns it into `UnknownHostKey`.
        let accept = self.pinned.as_deref() == Some(fp.as_str());
        *self.seen.lock().unwrap_or_else(|p| p.into_inner()) = Some(fp);
        Ok(accept)
    }
}

type SshHandle = client::Handle<HostKeyHandler>;

#[async_trait]
impl TunnelProvider for SshTunnelProvider {
    async fn open(
        &self,
        profile_id: &str,
        cfg: &TunnelConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<TunnelHandle> {
        let TunnelConfig::Ssh {
            host,
            port,
            username,
            auth,
            known_host_fingerprint,
        } = cfg;
        if host.trim().is_empty() {
            return Err(CoreError::Invalid("SSH tunnel host is empty".into()));
        }
        if username.trim().is_empty() {
            return Err(CoreError::Invalid("SSH tunnel username is empty".into()));
        }
        if target_host.trim().is_empty() || target_port == 0 {
            return Err(CoreError::Invalid(
                "tunnel target host/port is not set".into(),
            ));
        }

        let pinned = known_host_fingerprint
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let seen = Arc::new(Mutex::new(None));
        let config = Arc::new(client::Config {
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            keepalive_max: KEEPALIVE_MAX,
            nodelay: true,
            ..Default::default()
        });

        // 1. Connect + key exchange. The host key is checked here, and only a key equal to the
        //    pin lets the exchange complete. Nothing below runs for an unpinned or changed key.
        let handler = HostKeyHandler {
            pinned: pinned.clone(),
            seen: seen.clone(),
        };
        let connect = tokio::time::timeout(
            CONNECT_TIMEOUT,
            client::connect(config, (host.as_str(), *port), handler),
        )
        .await;
        let mut session: SshHandle = match connect {
            Err(_) => {
                return Err(CoreError::Tunnel(format!(
                    "timed out connecting to SSH host {host}:{port}"
                )));
            }
            Ok(Ok(session)) => session,
            Ok(Err(err)) => {
                let presented = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
                return Err(connect_error(
                    err,
                    host,
                    *port,
                    pinned.as_deref(),
                    presented.as_deref(),
                ));
            }
        };

        // 2. Authenticate.
        let authed = tokio::time::timeout(
            AUTH_TIMEOUT,
            self.authenticate(&mut session, profile_id, username, auth),
        )
        .await;
        match authed {
            Err(_) => {
                let _ = session
                    .disconnect(russh::Disconnect::ByApplication, "", "en")
                    .await;
                return Err(CoreError::Auth(format!(
                    "SSH authentication to {host}:{port} timed out"
                )));
            }
            Ok(Err(e)) => {
                let _ = session
                    .disconnect(russh::Disconnect::ByApplication, "", "en")
                    .await;
                return Err(e);
            }
            Ok(Ok(())) => {}
        }

        // 3. Local listener + accept loop.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| {
                CoreError::Tunnel(format!("cannot open a local port for the tunnel: {e}"))
            })?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| CoreError::Tunnel(e.to_string()))?;
        let cancel = CancellationToken::new();
        tokio::spawn(accept_loop(
            listener,
            session,
            cancel.clone(),
            target_host.to_string(),
            target_port,
        ));
        tracing::debug!(%local_addr, target_host, target_port, "ssh tunnel listening");
        Ok(TunnelHandle { local_addr, cancel })
    }
}

fn connect_error(
    err: russh::Error,
    host: &str,
    port: u16,
    pinned: Option<&str>,
    presented: Option<&str>,
) -> CoreError {
    match (pinned, presented) {
        (Some(pinned), Some(presented)) if pinned != presented => {
            return CoreError::Tunnel(format!(
                "host key mismatch for {host}:{port}: expected {pinned} but the server presented {presented}. \
                 If the server was legitimately re-keyed, clear the saved host key fingerprint in the connection's SSH \
                 settings; otherwise this could be a man-in-the-middle attack"
            ));
        }
        (None, Some(presented)) => {
            return CoreError::UnknownHostKey(PresentedHostKey {
                host: host.to_string(),
                port,
                fingerprint: presented.to_string(),
            });
        }
        _ => {}
    }
    match err {
        russh::Error::IO(io) => {
            CoreError::Tunnel(format!("cannot reach SSH host {host}:{port}: {io}"))
        }
        other => CoreError::Tunnel(format!("SSH handshake with {host}:{port} failed: {other}")),
    }
}

impl SshTunnelProvider {
    async fn authenticate(
        &self,
        session: &mut SshHandle,
        profile_id: &str,
        user: &str,
        auth: &SshAuth,
    ) -> Result<()> {
        match auth {
            SshAuth::Agent => auth_with_agent(session, user).await,
            SshAuth::PrivateKey { path } => {
                let passphrase = self
                    .vault
                    .get(&vault_keys::ssh_passphrase(profile_id))
                    .await?;
                auth_with_key_file(session, user, path, passphrase).await
            }
            SshAuth::Password => {
                let Some(password) = self
                    .vault
                    .get(&vault_keys::ssh_password(profile_id))
                    .await?
                else {
                    return Err(CoreError::Auth(
                        "no SSH password is saved for this connection".into(),
                    ));
                };
                auth_with_password(session, user, password.expose_secret()).await
            }
        }
    }
}

fn rejected(user: &str, what: &str) -> CoreError {
    CoreError::Auth(format!("SSH server rejected {what} for user \"{user}\""))
}

fn ssh_err(context: &str, e: impl std::fmt::Display) -> CoreError {
    CoreError::Tunnel(format!("{context}: {e}"))
}

async fn auth_with_password(session: &mut SshHandle, user: &str, password: &str) -> Result<()> {
    let result = session
        .authenticate_password(user, password)
        .await
        .map_err(|e| ssh_err("SSH password authentication failed", e))?;
    if result.success() {
        return Ok(());
    }
    // PAM-only servers expose keyboard-interactive instead of `password`.
    let kbd_allowed = match &result {
        russh::client::AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods
            .iter()
            .any(|m| matches!(m, russh::MethodKind::KeyboardInteractive)),
        russh::client::AuthResult::Success => false,
    };
    if !kbd_allowed {
        return Err(rejected(user, "the password"));
    }
    let mut reply = session
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|e| ssh_err("SSH keyboard-interactive authentication failed", e))?;
    for _ in 0..4 {
        match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                return Err(rejected(user, "the password"));
            }
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                // Every prompt is answered with the password; a server asking anything else
                // (OTP, …) will fail and we report a rejection.
                let answers = prompts.iter().map(|_| password.to_string()).collect();
                reply = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|e| ssh_err("SSH keyboard-interactive authentication failed", e))?;
            }
        }
    }
    Err(rejected(user, "the password (keyboard-interactive)"))
}

async fn auth_with_key_file(
    session: &mut SshHandle,
    user: &str,
    path: &str,
    passphrase: Option<secrecy::SecretString>,
) -> Result<()> {
    let expanded = expand_tilde(path)?;
    let shown = expanded.display().to_string();
    let load_path = expanded.clone();
    let key = tokio::task::spawn_blocking(move || {
        // Key derivation for encrypted keys is CPU heavy; keep it off the async threads.
        russh::keys::load_secret_key(&load_path, passphrase.as_ref().map(|p| p.expose_secret()))
    })
    .await
    .map_err(|e| CoreError::Tunnel(format!("key loading task failed: {e}")))?
    .map_err(|e| match e {
        russh::keys::Error::KeyIsEncrypted => CoreError::Auth(format!(
            "SSH key {shown} is passphrase-protected; save its passphrase in the connection's SSH settings"
        )),
        russh::keys::Error::IO(io) => CoreError::Auth(format!("cannot read SSH key {shown}: {io}")),
        other => CoreError::Auth(format!("cannot use SSH key {shown} (wrong passphrase or unsupported format?): {other}")),
    })?;

    let hash_alg = if key.algorithm().is_rsa() {
        rsa_hash(session).await
    } else {
        None
    };
    let result = session
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg))
        .await
        .map_err(|e| ssh_err("SSH public-key authentication failed", e))?;
    if result.success() {
        Ok(())
    } else {
        Err(rejected(user, &format!("the key {shown}")))
    }
}

/// Best RSA signature hash the server supports (`rsa-sha2-512` > `-256` > legacy `ssh-rsa`).
async fn rsa_hash(session: &SshHandle) -> Option<HashAlg> {
    session
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten()
}

fn expand_tilde(path: &str) -> Result<std::path::PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(CoreError::Invalid("SSH private key path is empty".into()));
    }
    if trimmed == "~" {
        return dirs::home_dir().ok_or_else(|| {
            CoreError::Invalid("cannot determine the home directory to expand '~'".into())
        });
    }
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        let home = dirs::home_dir().ok_or_else(|| {
            CoreError::Invalid("cannot determine the home directory to expand '~'".into())
        })?;
        return Ok(home.join(rest));
    }
    Ok(std::path::PathBuf::from(trimmed))
}

/// Connects to the platform's ssh-agent as a type-erased client.
async fn connect_agent()
-> Result<AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin>>> {
    #[cfg(unix)]
    {
        match AgentClient::connect_env().await {
            Ok(client) => Ok(client.dynamic()),
            Err(russh::keys::Error::EnvVar(_)) => Err(CoreError::Auth(
                "SSH_AUTH_SOCK is not set: start ssh-agent (and ssh-add your key) or pick another SSH auth method".into(),
            )),
            Err(russh::keys::Error::BadAuthSock) => Err(CoreError::Auth(
                "SSH_AUTH_SOCK points at a socket that doesn't exist: is ssh-agent running?".into(),
            )),
            Err(e) => Err(CoreError::Auth(format!("cannot talk to ssh-agent: {e}"))),
        }
    }
    #[cfg(windows)]
    {
        match AgentClient::connect_named_pipe(WINDOWS_AGENT_PIPE).await {
            Ok(client) => Ok(client.dynamic()),
            Err(e) => Err(CoreError::Auth(format!(
                "cannot reach the Windows OpenSSH agent ({WINDOWS_AGENT_PIPE}): {e}. \
                 Start the \"OpenSSH Authentication Agent\" service and add your key with ssh-add"
            ))),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(CoreError::Auth(
            "ssh-agent is not supported on this platform".into(),
        ))
    }
}

async fn auth_with_agent(session: &mut SshHandle, user: &str) -> Result<()> {
    use russh::keys::agent::AgentIdentity;

    let mut agent = connect_agent().await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| CoreError::Auth(format!("ssh-agent refused to list its keys: {e}")))?;
    if identities.is_empty() {
        return Err(CoreError::Auth(
            "ssh-agent has no keys loaded (run ssh-add)".into(),
        ));
    }
    let total = identities.len();
    for identity in identities {
        let result = match &identity {
            AgentIdentity::PublicKey { key, .. } => {
                let hash_alg = if key.algorithm().is_rsa() {
                    rsa_hash(session).await
                } else {
                    None
                };
                session
                    .authenticate_publickey_with(user, key.clone(), hash_alg, &mut agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                let hash_alg = if certificate.public_key().algorithm().is_rsa() {
                    rsa_hash(session).await
                } else {
                    None
                };
                session
                    .authenticate_certificate_with(user, certificate.clone(), hash_alg, &mut agent)
                    .await
            }
        };
        match result {
            Ok(r) if r.success() => return Ok(()),
            Ok(_) => tracing::debug!(
                comment = identity.comment(),
                "server declined an ssh-agent key"
            ),
            // The agent declined to sign (e.g. confirmation refused). Try the next key.
            Err(e) => {
                tracing::debug!(comment = identity.comment(), error = %e, "ssh-agent could not sign")
            }
        }
    }
    Err(CoreError::Auth(format!(
        "SSH server rejected all {total} key(s) offered by ssh-agent for user \"{user}\""
    )))
}

/// Accepts local connections and forwards each over its own `direct-tcpip` channel.
async fn accept_loop(
    listener: TcpListener,
    session: SshHandle,
    cancel: CancellationToken,
    target_host: String,
    target_port: u16,
) {
    let session = Arc::new(session);
    let mut liveness = tokio::time::interval(LIVENESS_POLL);
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = liveness.tick() => {
                if session.is_closed() {
                    tracing::warn!("SSH session closed; shutting the tunnel down");
                    break;
                }
            }
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        // EMFILE and friends are transient; don't spin.
                        tracing::warn!(error = %e, "tunnel accept failed");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                };
                let session = session.clone();
                let host = target_host.clone();
                let token = cancel.child_token();
                tokio::spawn(async move {
                    if let Err(e) = forward(socket, peer, &session, &host, target_port, token).await {
                        tracing::debug!(error = %e, "tunnel connection ended with error");
                    }
                });
            }
        }
    }
    // Stop accepting, tear down in-flight pipes, and say goodbye to the bastion.
    cancel.cancel();
    drop(listener);
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "tunnel closed", "en")
        .await;
}

async fn forward(
    mut socket: TcpStream,
    peer: SocketAddr,
    session: &SshHandle,
    host: &str,
    port: u16,
    cancel: CancellationToken,
) -> std::result::Result<(), String> {
    let _ = socket.set_nodelay(true);
    let channel = session
        .channel_open_direct_tcpip(
            host,
            u32::from(port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await
        .map_err(|e| format!("bastion refused to forward to {host}:{port}: {e}"))?;
    let mut stream = channel.into_stream();
    tokio::select! {
        _ = cancel.cancelled() => Ok(()),
        r = tokio::io::copy_bidirectional(&mut socket, &mut stream) => r.map(|_| ()).map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conn_tilde_expansion() {
        assert!(expand_tilde("").is_err());
        assert_eq!(
            expand_tilde("/etc/key").unwrap(),
            std::path::PathBuf::from("/etc/key")
        );
        if let Some(home) = dirs::home_dir() {
            assert_eq!(
                expand_tilde("~/.ssh/id_ed25519").unwrap(),
                home.join(".ssh/id_ed25519")
            );
            assert_eq!(expand_tilde("~").unwrap(), home);
        }
        // `~user/…` is not expanded.
        assert_eq!(
            expand_tilde("~bob/key").unwrap(),
            std::path::PathBuf::from("~bob/key")
        );
    }

    #[test]
    fn conn_mismatch_error_is_explicit() {
        let e = connect_error(
            russh::Error::UnknownKey,
            "bastion",
            22,
            Some("SHA256:aaa"),
            Some("SHA256:bbb"),
        );
        let msg = e.to_string();
        assert!(msg.contains("host key mismatch"), "{msg}");
        assert!(msg.contains("SHA256:aaa") && msg.contains("SHA256:bbb"));
        assert_eq!(e.code(), "tunnel");
    }

    #[test]
    fn conn_unpinned_host_is_reported_with_the_key_it_presented() {
        let e = connect_error(
            russh::Error::UnknownKey,
            "bastion",
            2222,
            None,
            Some("SHA256:bbb"),
        );
        assert_eq!(e.code(), "unknown_host_key");
        match &e {
            CoreError::UnknownHostKey(key) => assert_eq!(
                (key.host.as_str(), key.port, key.fingerprint.as_str()),
                ("bastion", 2222, "SHA256:bbb")
            ),
            other => panic!("{other:?}"),
        }
        let payload = crate::error::ErrorPayload::from(&e);
        assert_eq!(payload.host_key.unwrap().fingerprint, "SHA256:bbb");
        assert!(e.to_string().contains("bastion:2222") && e.to_string().contains("SHA256:bbb"));

        // Before the server has presented a key there is nothing to trust: an ordinary failure.
        let e = connect_error(russh::Error::Disconnect, "bastion", 22, None, None);
        assert_eq!(e.code(), "tunnel");
    }

    #[tokio::test]
    async fn conn_only_the_pinned_key_passes_the_host_key_check() {
        use russh::client::Handler;
        use russh::keys::PublicKey;
        use russh::keys::ssh_key::public::{Ed25519PublicKey, KeyData};

        // Any 32 bytes encode as an Ed25519 public key; the check only compares fingerprints.
        let public = PublicKeyOrCertificate::PublicKey {
            key: PublicKey::new(KeyData::Ed25519(Ed25519PublicKey([7; 32])), ""),
            hash_alg: None,
        };
        let fp = fingerprint_of(&public);
        assert!(crate::model::is_host_key_fingerprint(&fp), "{fp}");
        for (pinned, accepted) in [
            (Some(fp.clone()), true),
            (None, false),
            (
                Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()),
                false,
            ),
        ] {
            let seen = Arc::new(Mutex::new(None));
            let mut h = HostKeyHandler {
                pinned: pinned.clone(),
                seen: seen.clone(),
            };
            assert_eq!(
                h.check_server_key(&public).await.unwrap(),
                accepted,
                "{pinned:?}"
            );
            assert_eq!(seen.lock().unwrap().as_deref(), Some(fp.as_str()));
        }
    }

    #[test]
    fn conn_matching_pin_is_not_reported_as_mismatch() {
        let e = connect_error(
            russh::Error::Disconnect,
            "bastion",
            22,
            Some("SHA256:aaa"),
            Some("SHA256:aaa"),
        );
        assert!(!e.to_string().contains("mismatch"));
    }
}
