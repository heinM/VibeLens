//! Import connections from other database tools, so switching to VibeLens does not start with
//! retyping every connection.
//!
//! | Source              | Where                                                              |
//! |---------------------|--------------------------------------------------------------------|
//! | DataGrip / IntelliJ | `<project>/.idea/dataSources.xml` (+ `dataSources.local.xml` for user names), global data sources under the IDE config `options/`, SSH settings from `options/sshConfigs.xml`, or XML copied with *Copy* in the Database Explorer |
//! | DBeaver             | `DBeaverData/workspace6/<project>/.dbeaver/data-sources*.json`    |
//! | Azure Data Studio   | `azuredatastudio/User/settings.json` (`datasource.connections`, PostgreSQL extension) |
//! | pgAdmin 4           | a `servers.json` export (*Tools → Import/Export Servers*)           |
//! | libpq               | `~/.pg_service.conf` / `$PGSERVICEFILE`                             |
//!
//! Only Postgres-wire and Redis/Valkey connections are imported; everything else is reported as
//! skipped with the reason. **Passwords are never imported**: the source tools keep them in the
//! OS keychain, KeePass or their own encrypted stores, and VibeLens does not decrypt other
//! applications' secrets. The user adds each password in the connection editor (connecting
//! without one fails with "no password saved ... edit the connection to add one"). Usernames stored only in an encrypted store (DBeaver's
//! `credentials-config.json`) are therefore missing too, and the candidate says so.
//!
//! Parsing is pure (text in, candidates out) so every format is unit-tested from fixtures;
//! [`discover`] is the only part that touches the file system.

mod ads;
mod datagrip;
mod dbeaver;
mod jdbc;
mod pgadmin;
mod pgservice;
mod xml;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::model::*;

/// Largest file the importer reads (these are small config files; anything bigger is not one).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ImportSource {
    DataGrip,
    DBeaver,
    AzureDataStudio,
    PgAdmin,
    PgService,
}

impl ImportSource {
    pub fn label(self) -> &'static str {
        match self {
            ImportSource::DataGrip => "DataGrip / IntelliJ",
            ImportSource::DBeaver => "DBeaver",
            ImportSource::AzureDataStudio => "Azure Data Studio",
            ImportSource::PgAdmin => "pgAdmin 4",
            ImportSource::PgService => "pg_service.conf",
        }
    }
}

/// One connection found in a source file, mapped to a ready-to-save profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub source: ImportSource,
    pub source_file: String,
    /// The folder / group / project it lives in at the source, for display and name prefixes.
    pub group: Option<String>,
    pub profile: ConnectionProfile,
    /// Things that did not carry over exactly (TLS mode approximated, SSH not found, ...).
    pub warnings: Vec<String>,
    /// The connection needs a secret VibeLens could not import (a password, an SSH password).
    pub needs_secret: bool,
    /// Id of an existing VibeLens profile with the same engine, endpoint, database and user.
    pub duplicate_of: Option<ProfileId>,
}

/// An entry that was recognised but not imported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkippedEntry {
    pub name: String,
    pub reason: String,
}

/// Everything one source file yielded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub source: ImportSource,
    pub path: String,
    pub candidates: Vec<ImportCandidate>,
    pub skipped: Vec<SkippedEntry>,
    /// Set when the file could not be read or parsed at all.
    pub error: Option<String>,
}

/// A file [`discover`] found, before parsing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FoundFile {
    pub source: ImportSource,
    pub path: PathBuf,
}

// ---- intermediate form -------------------------------------------------------------------------

/// What every parser produces; [`Draft::into_candidate`] turns it into a profile.
#[derive(Debug, Default, Clone)]
pub(crate) struct Draft {
    pub name: String,
    pub engine: Option<EngineKind>,
    pub host: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    /// libpq `sslmode` (or the closest the source has).
    pub sslmode: Option<String>,
    pub ca_cert: Option<String>,
    pub ssh: Option<SshDraft>,
    /// Explicit environment from the source (DBeaver connection types).
    pub environment: Option<Environment>,
    pub group: Option<String>,
    pub color: Option<String>,
    pub auth: DraftAuth,
    pub warnings: Vec<String>,
    /// The source keeps a password for this connection that is not imported.
    pub password_saved: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum DraftAuth {
    #[default]
    Password,
    /// No credentials (trust auth, Redis without AUTH).
    None,
    /// RDS IAM in the source.
    RdsIam,
    /// Microsoft Entra ID (Azure AD) token auth in the source.
    Entra,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SshDraft {
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub auth: SshAuth,
}

/// Command that prints an Entra access token for Azure Database for PostgreSQL.
pub const AZURE_PG_TOKEN_COMMAND: &str =
    "az account get-access-token --resource-type oss-rdbms --query accessToken -o tsv";

fn is_loopback(host: &str) -> bool {
    let h = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h == "::1"
        || h.starts_with("127.")
        || h == "host.docker.internal"
        || h.starts_with('/')
}

/// Maps a libpq `sslmode` onto VibeLens's three TLS modes.
fn tls_for(sslmode: Option<&str>, host: &str, ca: Option<String>) -> (TlsConfig, Option<String>) {
    let rds = host.to_ascii_lowercase().ends_with(".rds.amazonaws.com");
    let mode = sslmode.map(|m| m.trim().to_ascii_lowercase());
    let (mode, warning) = match mode.as_deref() {
        Some("disable") => (TlsMode::Disable, None),
        Some("require") => (TlsMode::Require, None),
        Some("verify-full") => (TlsMode::VerifyFull, None),
        Some("verify-ca") => (
            TlsMode::VerifyFull,
            Some(
                "TLS was verify-ca; VibeLens verifies the host name too (verify-full)".to_string(),
            ),
        ),
        // `allow`, `prefer` or nothing: the libpq/JDBC default "try TLS, fall back to plaintext"
        // has no safe equivalent. Local servers rarely have TLS; remote ones get verification.
        _ if is_loopback(host) => (TlsMode::Disable, None),
        other => (
            TlsMode::VerifyFull,
            Some(format!(
                "TLS mode was {}; using verify-full. Switch to Require if the server's certificate is not trusted",
                other.unwrap_or("not set")
            )),
        ),
    };
    let ca_cert_path = ca.filter(|c| !c.trim().is_empty());
    (
        TlsConfig {
            mode,
            ca_cert_path,
            use_rds_bundle: rds && mode == TlsMode::VerifyFull,
        },
        warning,
    )
}

/// Words in a name or folder that say which environment a connection belongs to.
fn environment_hint(texts: &[&str]) -> Option<Environment> {
    let words: Vec<String> = texts
        .iter()
        .flat_map(|t| {
            t.split(|c: char| !c.is_ascii_alphanumeric())
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .collect();
    let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    if has(&["prod", "production", "prd", "live"]) {
        Some(Environment::Prod)
    } else if has(&["staging", "stage", "stg", "uat", "preprod"]) {
        Some(Environment::Staging)
    } else if has(&["test", "testing", "qa", "tst", "sit"]) {
        Some(Environment::Test)
    } else if has(&["dev", "development", "local", "localhost"]) {
        Some(Environment::Dev)
    } else {
        None
    }
}

/// Region of an RDS/Aurora endpoint (`x.cluster-abc.eu-west-1.rds.amazonaws.com`).
fn rds_region(host: &str) -> Option<String> {
    let h = host.to_ascii_lowercase();
    let rest = h.strip_suffix(".rds.amazonaws.com")?;
    rest.rsplit('.').next().map(String::from)
}

fn normalise_color(c: Option<&str>) -> Option<String> {
    let c = c?.trim();
    let hex = c.strip_prefix('#').unwrap_or(c);
    if (hex.len() == 6 || hex.len() == 3) && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(format!("#{}", hex.to_ascii_lowercase()))
    } else {
        None
    }
}

impl Draft {
    pub(crate) fn into_candidate(self, source: ImportSource, file: &str) -> ImportCandidate {
        let engine = self.engine.unwrap_or(EngineKind::Postgres);
        let mut warnings = self.warnings;
        let host = self.host.trim().to_string();
        let port = self.port.filter(|p| *p > 0).unwrap_or(match engine {
            EngineKind::Valkey => 6379,
            _ => 5432,
        });
        let (tls, tls_warning) = match engine {
            // Redis URLs carry TLS in their scheme; the parsers set sslmode accordingly.
            EngineKind::Valkey => tls_for(
                Some(self.sslmode.as_deref().unwrap_or("disable")),
                &host,
                self.ca_cert,
            ),
            _ => tls_for(self.sslmode.as_deref(), &host, self.ca_cert),
        };
        warnings.extend(tls_warning);
        let user = self
            .user
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());

        let mut needs_secret = false;
        let auth = match (&self.auth, engine) {
            (DraftAuth::None, _) => AuthMethod::None,
            (DraftAuth::RdsIam, EngineKind::Postgres) => {
                let region = rds_region(&host).unwrap_or_default();
                warnings.push(
                    "RDS IAM auth: pick the AWS identity (profile or SSO) in the editor".into(),
                );
                AuthMethod::RdsIam {
                    username: user.clone().unwrap_or_default(),
                    aws: AwsTarget {
                        identity: AwsIdentity::DefaultChain,
                        region,
                    },
                }
            }
            (DraftAuth::Entra, EngineKind::Postgres) => {
                warnings.push(format!(
                    "Microsoft Entra auth: VibeLens runs `{AZURE_PG_TOKEN_COMMAND}` for each new connection (needs the Azure CLI, logged in)"
                ));
                AuthMethod::AccessToken {
                    username: user.clone().unwrap_or_default(),
                    command: Some(AZURE_PG_TOKEN_COMMAND.to_string()),
                }
            }
            (_, EngineKind::Valkey) if user.is_none() && !self.password_saved => AuthMethod::None,
            _ => {
                needs_secret = true;
                AuthMethod::Password {
                    username: user.clone().unwrap_or_else(|| match engine {
                        EngineKind::Valkey => "default".into(),
                        _ => String::new(),
                    }),
                }
            }
        };
        if matches!(auth, AuthMethod::Password { ref username } if username.is_empty()) {
            warnings.push("no user name in the source: add it in the editor".into());
        }
        if needs_secret {
            warnings.push(format!(
                "passwords are not imported{}: add it in the connection editor",
                if self.password_saved {
                    format!(" (the password stays in {})", source.label())
                } else {
                    String::new()
                }
            ));
        }

        let tunnel = self.ssh.map(|s| {
            if matches!(s.auth, SshAuth::Password) {
                needs_secret = true;
            }
            TunnelConfig::Ssh {
                host: s.host.trim().to_string(),
                port: s.port.filter(|p| *p > 0).unwrap_or(22),
                username: s.user.unwrap_or_default(),
                auth: s.auth,
                known_host_fingerprint: None,
            }
        });

        let name = {
            let n = self.name.trim();
            if n.is_empty() {
                match &self.database {
                    Some(db) if !db.is_empty() => format!("{db}@{host}"),
                    _ => host.clone(),
                }
            } else {
                n.to_string()
            }
        };
        let environment = self
            .environment
            .or_else(|| environment_hint(&[&name, self.group.as_deref().unwrap_or(""), &host]))
            .unwrap_or_default();

        let profile = ConnectionProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            engine,
            host,
            port,
            database: match engine {
                EngineKind::Postgres => self.database.filter(|d| !d.trim().is_empty()),
                _ => None,
            },
            auth,
            tls,
            tunnel,
            environment,
            ai_data_access: AiDataAccess::default(),
            color: normalise_color(self.color.as_deref()),
            pg: PgOptions::default(),
            valkey: ValkeyOptions::default(),
            created_at: None,
            updated_at: None,
            iceberg: None,
        };
        ImportCandidate {
            source,
            source_file: file.to_string(),
            group: self.group.filter(|g| !g.trim().is_empty()),
            profile,
            warnings,
            needs_secret,
            duplicate_of: None,
        }
    }
}

// ---- entry points --------------------------------------------------------------------------

/// Parses `text` as `source`. `file` is only used for display (and to find DataGrip's sibling
/// `dataSources.local.xml` / `sshConfigs.xml`, which the caller passes as `extra`).
pub fn parse_text(source: ImportSource, text: &str, file: &str, extra: &Extra) -> ImportReport {
    let parsed = match source {
        ImportSource::DataGrip => datagrip::parse(text, extra),
        ImportSource::DBeaver => dbeaver::parse(text),
        ImportSource::AzureDataStudio => ads::parse(text),
        ImportSource::PgAdmin => pgadmin::parse(text),
        ImportSource::PgService => pgservice::parse(text),
    };
    match parsed {
        Ok((drafts, skipped)) => ImportReport {
            source,
            path: file.to_string(),
            candidates: drafts
                .into_iter()
                .map(|d| d.into_candidate(source, file))
                .collect(),
            skipped,
            error: None,
        },
        Err(e) => ImportReport {
            source,
            path: file.to_string(),
            candidates: Vec::new(),
            skipped: Vec::new(),
            error: Some(e),
        },
    }
}

/// Side files some formats need.
#[derive(Debug, Default, Clone)]
pub struct Extra {
    /// DataGrip `dataSources.local.xml` next to `dataSources.xml` (user names, SSH links).
    pub datagrip_local: Option<String>,
    /// DataGrip `sshConfigs.xml` files from the IDE config `options/` directories.
    pub datagrip_ssh_configs: Vec<String>,
}

/// Guesses the format of a file the user picked (or text they pasted).
pub fn detect(file_name: &str, text: &str) -> Option<ImportSource> {
    let name = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file_name)
        .to_ascii_lowercase();
    let t = text.trim_start();
    if name.starts_with("datasources") && name.ends_with(".xml")
        || (t.starts_with('<') && t.contains("<data-source"))
    {
        return Some(ImportSource::DataGrip);
    }
    if name.contains("pg_service") || (t.starts_with('[') && t.contains("host=")) {
        return Some(ImportSource::PgService);
    }
    if t.starts_with('{') {
        if t.contains("\"Servers\"") {
            return Some(ImportSource::PgAdmin);
        }
        if t.contains("datasource.connections") {
            return Some(ImportSource::AzureDataStudio);
        }
        if t.contains("\"connections\"")
            && (name.starts_with("data-sources") || t.contains("\"provider\""))
        {
            return Some(ImportSource::DBeaver);
        }
    }
    None
}

fn read_small(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| CoreError::Other(format!("cannot read {}: {e}", path.display())))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(CoreError::Invalid(format!(
            "{} is too large to be a connection file",
            path.display()
        )));
    }
    std::fs::read_to_string(path)
        .map_err(|e| CoreError::Other(format!("cannot read {}: {e}", path.display())))
}

/// Reads and parses one file. DataGrip's sibling files are picked up automatically.
pub fn parse_file(source: Option<ImportSource>, path: &Path, home: Option<&Path>) -> ImportReport {
    let display = path.display().to_string();
    let text = match read_small(path) {
        Ok(t) => t,
        Err(e) => {
            return ImportReport {
                source: source.unwrap_or(ImportSource::DataGrip),
                path: display,
                candidates: Vec::new(),
                skipped: Vec::new(),
                error: Some(e.to_string()),
            };
        }
    };
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Some(source) = source.or_else(|| detect(file_name, &text)) else {
        return ImportReport {
            source: ImportSource::DataGrip,
            path: display,
            candidates: Vec::new(),
            skipped: Vec::new(),
            error: Some(
                "not a file VibeLens can import connections from (DataGrip dataSources.xml, DBeaver data-sources.json, Azure Data Studio settings.json, pgAdmin servers.json, pg_service.conf)".into(),
            ),
        };
    };
    let mut extra = Extra::default();
    if source == ImportSource::DataGrip {
        if let Some(dir) = path.parent() {
            extra.datagrip_local = read_small(&dir.join("dataSources.local.xml")).ok();
        }
        extra.datagrip_ssh_configs = home
            .map(|h| {
                jetbrains_config_dirs(h)
                    .into_iter()
                    .filter_map(|d| read_small(&d.join("options").join("sshConfigs.xml")).ok())
                    .collect()
            })
            .unwrap_or_default();
    }
    parse_text(source, &text, &display, &extra)
}

// ---- discovery -------------------------------------------------------------------------------

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

fn files_matching(dir: &Path, pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| e.file_name().to_str().is_some_and(&pred))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// Per-OS application-data roots below `home`.
fn app_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        home.join("Library").join("Application Support"), // macOS
        home.join(".config"),                             // Linux (XDG)
        home.join("AppData").join("Roaming"),             // Windows
    ];
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        roots.push(PathBuf::from(x));
    }
    if let Some(x) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(x));
    }
    roots.dedup();
    roots
}

/// `JetBrains/<Product><version>` configuration directories.
fn jetbrains_config_dirs(home: &Path) -> Vec<PathBuf> {
    app_roots(home)
        .into_iter()
        .flat_map(|r| subdirs(&r.join("JetBrains")))
        .collect()
}

/// Looks in the usual places for every supported tool. Never fails; missing places are skipped.
pub fn discover(home: &Path) -> Vec<FoundFile> {
    let mut out: Vec<FoundFile> = Vec::new();
    let mut push = |source, path: PathBuf| {
        if path.is_file() && !out.iter().any(|f| f.path == path) {
            out.push(FoundFile { source, path });
        }
    };

    // DataGrip / IntelliJ projects.
    let idea = |p: &Path| p.join(".idea").join("dataSources.xml");
    for root in [
        "DataGripProjects",
        "IdeaProjects",
        "PycharmProjects",
        "GolandProjects",
        "WebstormProjects",
        "Projects",
    ] {
        let base = home.join(root);
        for project in subdirs(&base) {
            push(ImportSource::DataGrip, idea(&project));
            if root == "Projects" {
                for nested in subdirs(&project) {
                    push(ImportSource::DataGrip, idea(&nested));
                }
            }
        }
    }
    // Global data sources and the default project of every JetBrains IDE.
    for cfg in jetbrains_config_dirs(home) {
        for f in files_matching(&cfg.join("options"), |n| {
            n.starts_with("dataSources") && n.ends_with(".xml") && !n.contains(".local.")
        }) {
            push(ImportSource::DataGrip, f);
        }
        for project in subdirs(&cfg.join("projects")) {
            push(ImportSource::DataGrip, idea(&project));
        }
    }

    // DBeaver (Community/Enterprise share the layout).
    let dbeaver_roots = [
        home.join("Library").join("DBeaverData"),
        home.join(".local").join("share").join("DBeaverData"),
        home.join("AppData").join("Roaming").join("DBeaverData"),
    ];
    for root in dbeaver_roots {
        for project in subdirs(&root.join("workspace6")) {
            for f in files_matching(&project.join(".dbeaver"), |n| {
                n.starts_with("data-sources") && n.ends_with(".json")
            }) {
                push(ImportSource::DBeaver, f);
            }
        }
    }

    // Azure Data Studio.
    for root in app_roots(home) {
        push(
            ImportSource::AzureDataStudio,
            root.join("azuredatastudio")
                .join("User")
                .join("settings.json"),
        );
    }

    // pgAdmin keeps servers in SQLite; only an exported servers.json is readable. Look for the
    // usual export names in the home folder and Downloads.
    for dir in [
        home.to_path_buf(),
        home.join("Downloads"),
        home.join(".pgadmin"),
    ] {
        push(ImportSource::PgAdmin, dir.join("servers.json"));
    }

    // libpq service file.
    if let Some(f) = std::env::var_os("PGSERVICEFILE") {
        push(ImportSource::PgService, PathBuf::from(f));
    }
    push(ImportSource::PgService, home.join(".pg_service.conf"));
    if let Some(d) = std::env::var_os("PGSYSCONFDIR") {
        push(
            ImportSource::PgService,
            PathBuf::from(d).join("pg_service.conf"),
        );
    }
    out
}

/// Discovers and parses everything; files without importable connections are dropped.
pub fn scan(home: &Path) -> Vec<ImportReport> {
    discover(home)
        .into_iter()
        .map(|f| parse_file(Some(f.source), &f.path, Some(home)))
        .filter(|r| !r.candidates.is_empty() || !r.skipped.is_empty() || r.error.is_some())
        .collect()
}

// ---- reconciliation with existing profiles ---------------------------------------------------

fn auth_user(a: &AuthMethod) -> &str {
    match a {
        AuthMethod::Password { username }
        | AuthMethod::RdsIam { username, .. }
        | AuthMethod::AccessToken { username, .. }
        | AuthMethod::ElastiCacheIam { username, .. } => username,
        AuthMethod::None | AuthMethod::SecretsManager { .. } => "",
    }
}

fn same_target(a: &ConnectionProfile, b: &ConnectionProfile) -> bool {
    a.engine == b.engine
        && a.host.eq_ignore_ascii_case(&b.host)
        && a.port == b.port
        && a.database.as_deref().unwrap_or("postgres")
            == b.database.as_deref().unwrap_or("postgres")
        && auth_user(&a.auth) == auth_user(&b.auth)
}

/// Marks candidates that match an existing profile, and candidates repeated across sources
/// (the same connection in a DataGrip project *and* globally): only the first stays unmarked.
pub fn mark_duplicates(reports: &mut [ImportReport], existing: &[ConnectionProfile]) {
    let mut seen: Vec<ConnectionProfile> = Vec::new();
    for r in reports.iter_mut() {
        for c in r.candidates.iter_mut() {
            if let Some(e) = existing.iter().find(|e| same_target(e, &c.profile)) {
                c.duplicate_of = Some(e.id.clone());
            } else if let Some(prev) = seen.iter().find(|p| same_target(p, &c.profile)) {
                c.duplicate_of = Some(prev.id.clone());
            } else {
                seen.push(c.profile.clone());
            }
        }
    }
}

/// A name not yet used by `taken` (case-insensitive): `name`, `name (DataGrip)`, `name (2)`, ...
pub fn unique_name(name: &str, source: Option<ImportSource>, taken: &[String]) -> String {
    let free = |n: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    let base = name.trim();
    if free(base) {
        return base.to_string();
    }
    if let Some(s) = source {
        let tagged = format!("{base} ({})", s.label());
        if free(&tagged) {
            return tagged;
        }
    }
    (2..)
        .map(|i| format!("{base} ({i})"))
        .find(|n| free(n))
        .expect("an unused name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_mapping() {
        let t = |m: Option<&str>, h: &str| tls_for(m, h, None);
        assert_eq!(t(Some("disable"), "db").0.mode, TlsMode::Disable);
        assert_eq!(t(Some("require"), "db").0.mode, TlsMode::Require);
        assert_eq!(t(Some("verify-full"), "db").0.mode, TlsMode::VerifyFull);
        let (c, w) = t(Some("verify-ca"), "db");
        assert_eq!(c.mode, TlsMode::VerifyFull);
        assert!(w.is_some());
        assert_eq!(t(None, "localhost").0.mode, TlsMode::Disable);
        assert_eq!(t(Some("prefer"), "127.0.0.1").0.mode, TlsMode::Disable);
        let (c, w) = t(Some("prefer"), "db.example.com");
        assert_eq!(c.mode, TlsMode::VerifyFull);
        assert!(w.unwrap().contains("prefer"));
        let (c, _) = t(
            Some("verify-full"),
            "x.cluster-abc.eu-west-1.rds.amazonaws.com",
        );
        assert!(c.use_rds_bundle);
    }

    #[test]
    fn environment_hints() {
        assert_eq!(environment_hint(&["orders-prod"]), Some(Environment::Prod));
        assert_eq!(
            environment_hint(&["Orders", "UAT"]),
            Some(Environment::Staging)
        );
        assert_eq!(environment_hint(&["qa db"]), Some(Environment::Test));
        assert_eq!(
            environment_hint(&["product-catalog"]),
            None,
            "whole words only"
        );
        assert_eq!(
            rds_region("a.cluster-x.af-south-1.rds.amazonaws.com").as_deref(),
            Some("af-south-1")
        );
    }

    #[test]
    fn unique_names() {
        let taken = vec!["orders".to_string(), "Orders (DBeaver)".to_string()];
        assert_eq!(unique_name("new", None, &taken), "new");
        assert_eq!(
            unique_name("ORDERS", Some(ImportSource::DataGrip), &taken),
            "ORDERS (DataGrip / IntelliJ)"
        );
        assert_eq!(
            unique_name("orders", Some(ImportSource::DBeaver), &taken),
            "orders (2)"
        );
    }

    #[test]
    fn detects_formats() {
        assert_eq!(
            detect("dataSources.xml", "<project/>"),
            Some(ImportSource::DataGrip)
        );
        assert_eq!(
            detect("clip.txt", "<data-source name='x'/>"),
            Some(ImportSource::DataGrip)
        );
        assert_eq!(
            detect("servers.json", r#"{"Servers":{}}"#),
            Some(ImportSource::PgAdmin)
        );
        assert_eq!(
            detect("settings.json", r#"{"datasource.connections":[]}"#),
            Some(ImportSource::AzureDataStudio)
        );
        assert_eq!(
            detect("data-sources.json", r#"{"connections":{}}"#),
            Some(ImportSource::DBeaver)
        );
        assert_eq!(
            detect(".pg_service.conf", "[a]\nhost=x"),
            Some(ImportSource::PgService)
        );
        assert_eq!(detect("notes.txt", "hello"), None);
    }

    #[test]
    fn duplicates_against_existing_and_across_sources() {
        let mk = |host: &str| {
            Draft {
                name: "x".into(),
                host: host.into(),
                database: Some("app".into()),
                user: Some("u".into()),
                ..Default::default()
            }
            .into_candidate(ImportSource::DBeaver, "f")
        };
        let existing = mk("a.example.com").profile;
        let mut reports = vec![
            ImportReport {
                source: ImportSource::DBeaver,
                path: "f".into(),
                candidates: vec![mk("a.example.com"), mk("b.example.com")],
                skipped: vec![],
                error: None,
            },
            ImportReport {
                source: ImportSource::DataGrip,
                path: "g".into(),
                candidates: vec![mk("B.example.com")],
                skipped: vec![],
                error: None,
            },
        ];
        mark_duplicates(&mut reports, std::slice::from_ref(&existing));
        assert_eq!(
            reports[0].candidates[0].duplicate_of.as_deref(),
            Some(existing.id.as_str())
        );
        assert!(reports[0].candidates[1].duplicate_of.is_none());
        assert_eq!(
            reports[1].candidates[0].duplicate_of.as_deref(),
            Some(reports[0].candidates[1].profile.id.as_str())
        );
    }

    #[test]
    fn discovers_files_in_a_fake_home() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let w = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w(
            h.join("DataGripProjects/shop/.idea/dataSources.xml"),
            "<project/>",
        );
        w(
            h.join("Library/Application Support/JetBrains/DataGrip2026.2/options/dataSources.xml"),
            "<x/>",
        );
        w(h.join("Library/Application Support/JetBrains/DataGrip2026.2/options/dataSources.local.xml"), "<x/>");
        w(
            h.join("Library/DBeaverData/workspace6/General/.dbeaver/data-sources.json"),
            "{}",
        );
        w(h.join(".config/azuredatastudio/User/settings.json"), "{}");
        w(h.join(".pg_service.conf"), "[a]\nhost=x\n");
        let found = discover(h);
        let has = |s: ImportSource, tail: &str| {
            found.iter().any(|f| {
                f.source == s && f.path.to_string_lossy().replace('\\', "/").ends_with(tail)
            })
        };
        assert!(has(ImportSource::DataGrip, "shop/.idea/dataSources.xml"));
        assert!(has(ImportSource::DataGrip, "options/dataSources.xml"));
        assert!(
            !found
                .iter()
                .any(|f| f.path.to_string_lossy().contains(".local."))
        );
        assert!(has(ImportSource::DBeaver, "data-sources.json"));
        assert!(has(ImportSource::AzureDataStudio, "settings.json"));
        assert!(has(ImportSource::PgService, ".pg_service.conf"));
    }
}
