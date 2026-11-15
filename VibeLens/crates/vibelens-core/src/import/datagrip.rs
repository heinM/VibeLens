//! DataGrip / IntelliJ data sources.
//!
//! - `dataSources.xml`: `<data-source name uuid group>` with `<driver-ref>` and `<jdbc-url>`.
//! - `dataSources.local.xml` (per user, keyed by `uuid`): `<user-name>`, `<secret-storage>`,
//!   `<ssh-properties>` (a link to `sshConfigs.xml`, or inline settings in older versions) and
//!   `<ssl-config>`. Newer DataGrip versions keep the user name only here.
//! - `sshConfigs.xml` (IDE config `options/`): `<sshConfig id host port username authType keyPath>`.
//! - The *Copy* action's clipboard format: `#DataSourceSettings#` header lines around
//!   `<data-source>` elements carrying the local fields inline.

use super::xml::{self, El};
use super::{Draft, DraftAuth, Extra, SkippedEntry, SshDraft, jdbc};
use crate::model::{EngineKind, SshAuth};

/// Fields merged from `dataSources.local.xml` (or inline in a clipboard snippet).
#[derive(Default)]
struct Local<'a> {
    user: Option<&'a str>,
    password_saved: bool,
    ssh: Option<&'a El>,
    ssl: Option<&'a El>,
}

fn local_fields(el: &El) -> Local<'_> {
    Local {
        user: el.child_text("user-name"),
        password_saved: el.child_text("secret-storage").is_some(),
        ssh: el.child("ssh-properties"),
        ssl: el.child("ssl-config"),
    }
}

fn expand_home(path: &str) -> String {
    path.replace("$USER_HOME$", "~")
}

fn ssh_from_config(cfg: &El) -> Option<SshDraft> {
    let host = cfg.attr("host").filter(|h| !h.trim().is_empty())?;
    let auth = match cfg
        .attr("authType")
        .unwrap_or("")
        .to_ascii_uppercase()
        .as_str()
    {
        "PASSWORD" => SshAuth::Password,
        "KEY_PAIR" => match cfg.attr("keyPath").filter(|k| !k.trim().is_empty()) {
            Some(k) => SshAuth::PrivateKey {
                path: expand_home(k),
            },
            None => SshAuth::Agent,
        },
        _ => SshAuth::Agent, // OPEN_SSH: agent / ~/.ssh/config
    };
    Some(SshDraft {
        host: host.to_string(),
        port: cfg.attr("port").and_then(|p| p.trim().parse().ok()),
        user: cfg
            .attr("username")
            .filter(|u| !u.trim().is_empty())
            .map(String::from),
        auth,
    })
}

/// Resolves `<ssh-properties>`: a reference to an `sshConfigs.xml` entry, or inline fields.
fn ssh_for(props: &El, configs: &[El], warnings: &mut Vec<String>) -> Option<SshDraft> {
    if props.child_text("enabled") != Some("true") {
        return None;
    }
    if let Some(id) = props.child_text("ssh-config-id") {
        let found = configs
            .iter()
            .flat_map(|root| root.find_all("sshConfig"))
            .find(|c| c.attr("id") == Some(id))
            .and_then(ssh_from_config);
        if found.is_none() {
            warnings.push(
                "uses an SSH tunnel whose settings were not found (DataGrip sshConfigs.xml); add the tunnel in the editor".into(),
            );
        }
        return found;
    }
    // Older inline form.
    let host = props
        .child_text("proxy-host")
        .or_else(|| props.child_text("host"))?;
    let auth = match props
        .child_text("auth-type")
        .unwrap_or("")
        .to_ascii_uppercase()
        .as_str()
    {
        "PASSWORD" => SshAuth::Password,
        "KEY_PAIR" => match props.child_text("key-file") {
            Some(k) => SshAuth::PrivateKey {
                path: expand_home(k),
            },
            None => SshAuth::Agent,
        },
        _ => SshAuth::Agent,
    };
    Some(SshDraft {
        host: host.to_string(),
        port: props.child_text("port").and_then(|p| p.parse().ok()),
        user: props.child_text("user").map(String::from),
        auth,
    })
}

fn sslmode_from(ssl: &El) -> Option<String> {
    if ssl.child_text("enabled") == Some("false") {
        return None;
    }
    let mode = ssl
        .child_text("mode")?
        .to_ascii_lowercase()
        .replace('_', "-");
    Some(mode)
}

pub(super) fn parse(text: &str, extra: &Extra) -> Result<(Vec<Draft>, Vec<SkippedEntry>), String> {
    // Clipboard snippets wrap the XML in `#...#` marker lines.
    let cleaned: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let root = xml::parse(&cleaned)?;
    let local_root = extra
        .datagrip_local
        .as_deref()
        .map(xml::parse)
        .transpose()?;
    let ssh_configs: Vec<El> = extra
        .datagrip_ssh_configs
        .iter()
        .filter_map(|s| xml::parse(s).ok())
        .collect();
    let locals: Vec<&El> = local_root
        .as_ref()
        .map(|r| r.find_all("data-source"))
        .unwrap_or_default();

    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    for ds in root.find_all("data-source") {
        let name = ds.attr("name").unwrap_or("").to_string();
        let Some(url) = ds.child_text("jdbc-url") else {
            // DDL data sources and other non-database entries have no URL.
            if ds.child_text("driver-ref").is_some() {
                skipped.push(SkippedEntry {
                    name,
                    reason: "no JDBC URL (DDL data source or URL kept elsewhere)".into(),
                });
            }
            continue;
        };
        let Some(target) = jdbc::parse(url) else {
            skipped.push(SkippedEntry {
                name,
                reason: format!("could not read the JDBC URL `{url}`"),
            });
            continue;
        };
        let driver = ds
            .child_text("driver-ref")
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut warnings = Vec::new();
        let engine = match target.scheme.as_str() {
            "postgresql" | "pgsql" => EngineKind::Postgres,
            "redshift" => {
                warnings.push(
                    "Amazon Redshift speaks the Postgres protocol but not every catalog query VibeLens runs works there".into(),
                );
                EngineKind::Postgres
            }
            "redis" | "rediss" => EngineKind::Valkey,
            other => {
                let what = if driver.is_empty() {
                    other
                } else {
                    driver.as_str()
                };
                skipped.push(SkippedEntry {
                    name,
                    reason: format!("{what} is not a database VibeLens supports (Postgres and Valkey/Redis only)"),
                });
                continue;
            }
        };
        if target.extra_hosts {
            warnings.push(format!(
                "several hosts were listed; only {} was kept",
                target.host
            ));
        }

        // Local fields: from the snippet itself, else from dataSources.local.xml by uuid.
        let inline = local_fields(ds);
        let uuid = ds.attr("uuid");
        let from_file = uuid
            .and_then(|u| locals.iter().find(|l| l.attr("uuid") == Some(u)))
            .map(|l| local_fields(l))
            .unwrap_or_default();
        let user = inline
            .user
            .or(from_file.user)
            .map(String::from)
            .or(target.user.clone());
        let password_saved = inline.password_saved || from_file.password_saved;
        let ssh = inline
            .ssh
            .or(from_file.ssh)
            .and_then(|p| ssh_for(p, &ssh_configs, &mut warnings));
        let ssl = inline.ssl.or(from_file.ssl);

        let sslmode = target
            .params
            .get("sslmode")
            .cloned()
            .or_else(|| ssl.and_then(sslmode_from))
            .or_else(|| (target.scheme == "rediss").then(|| "require".to_string()))
            .or_else(|| {
                (target.params.get("ssl").map(String::as_str) == Some("true"))
                    .then(|| "require".to_string())
            });
        let ca_cert = target
            .params
            .get("sslrootcert")
            .cloned()
            .or_else(|| ssl.and_then(|s| s.child_text("ca-cert")).map(expand_home));

        let auth_provider = ds
            .child_text("auth-provider")
            .unwrap_or("")
            .to_ascii_lowercase();
        let auth = if auth_provider.contains("aws") || auth_provider.contains("iam") {
            DraftAuth::RdsIam
        } else if auth_provider.contains("azure") || auth_provider.contains("entra") {
            DraftAuth::Entra
        } else if auth_provider.contains("no-auth") || auth_provider == "none" {
            DraftAuth::None
        } else {
            DraftAuth::Password
        };

        drafts.push(Draft {
            name,
            engine: Some(engine),
            host: target.host.clone(),
            port: target.port,
            database: match engine {
                EngineKind::Postgres => target.database.clone(),
                _ => None,
            },
            user,
            sslmode,
            ca_cert,
            ssh,
            environment: None,
            group: ds.attr("group").map(String::from),
            color: None,
            auth,
            warnings,
            password_saved,
        });
    }
    Ok((drafts, skipped))
}

#[cfg(test)]
mod tests {
    use super::super::{ImportSource, parse_text};
    use super::*;
    use crate::model::{AuthMethod, Environment, TlsMode, TunnelConfig};

    const DATA_SOURCES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="DataSourceManagerImpl" format="xml" multifile-model="true">
    <data-source source="LOCAL" name="orders@prod" group="Shop" uuid="u-1">
      <driver-ref>postgresql</driver-ref>
      <synchronize>true</synchronize>
      <jdbc-driver>org.postgresql.Driver</jdbc-driver>
      <jdbc-url>jdbc:postgresql://orders.cluster-x.af-south-1.rds.amazonaws.com:5432/orders?sslmode=verify-full</jdbc-url>
      <working-dir>$ProjectFileDir$</working-dir>
    </data-source>
    <data-source source="LOCAL" name="local pg" uuid="u-2">
      <driver-ref>postgresql</driver-ref>
      <jdbc-url>jdbc:postgresql://localhost:5432/postgres</jdbc-url>
    </data-source>
    <data-source source="LOCAL" name="cache" uuid="u-3">
      <driver-ref>redis</driver-ref>
      <jdbc-url>jdbc:redis://cache.internal:6380/0</jdbc-url>
    </data-source>
    <data-source source="LOCAL" name="legacy mysql" uuid="u-4">
      <driver-ref>mysql.8</driver-ref>
      <jdbc-url>jdbc:mysql://m:3306/x</jdbc-url>
    </data-source>
  </component>
</project>"#;

    const LOCAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="dataSourceStorageLocal" created-in="DB-252.1">
    <data-source name="orders@prod" uuid="u-1">
      <secret-storage>master_key</secret-storage>
      <user-name>app_ro</user-name>
      <ssh-properties>
        <enabled>true</enabled>
        <ssh-config-id>ssh-9</ssh-config-id>
      </ssh-properties>
    </data-source>
    <data-source name="local pg" uuid="u-2">
      <user-name>postgres</user-name>
    </data-source>
  </component>
</project>"#;

    const SSH: &str = r#"<application>
  <component name="SshConfigs">
    <configs>
      <sshConfig authType="KEY_PAIR" host="bastion.example.com" id="ssh-9" keyPath="$USER_HOME$/.ssh/id_ed25519" port="2222" username="ec2-user" />
    </configs>
  </component>
</application>"#;

    #[test]
    fn project_file_with_local_and_ssh_settings() {
        let extra = Extra {
            datagrip_local: Some(LOCAL.into()),
            datagrip_ssh_configs: vec![SSH.into()],
        };
        let r = parse_text(
            ImportSource::DataGrip,
            DATA_SOURCES,
            "/p/.idea/dataSources.xml",
            &extra,
        );
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(r.candidates.len(), 3);
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].reason.contains("mysql"));

        let prod = &r.candidates[0];
        let p = &prod.profile;
        assert_eq!(p.name, "orders@prod");
        assert_eq!(prod.group.as_deref(), Some("Shop"));
        assert_eq!(p.engine, EngineKind::Postgres);
        assert_eq!(p.database.as_deref(), Some("orders"));
        assert_eq!(
            p.auth,
            AuthMethod::Password {
                username: "app_ro".into()
            }
        );
        assert_eq!(p.tls.mode, TlsMode::VerifyFull);
        assert!(p.tls.use_rds_bundle);
        assert_eq!(p.environment, Environment::Prod, "from the name");
        assert!(prod.needs_secret);
        match &p.tunnel {
            Some(TunnelConfig::Ssh {
                host,
                port,
                username,
                auth,
                ..
            }) => {
                assert_eq!(
                    (host.as_str(), *port, username.as_str()),
                    ("bastion.example.com", 2222, "ec2-user")
                );
                assert_eq!(
                    auth,
                    &SshAuth::PrivateKey {
                        path: "~/.ssh/id_ed25519".into()
                    }
                );
            }
            other => panic!("{other:?}"),
        }

        let local = &r.candidates[1].profile;
        assert_eq!(
            local.tls.mode,
            TlsMode::Disable,
            "localhost without sslmode"
        );
        assert_eq!(
            local.auth,
            AuthMethod::Password {
                username: "postgres".into()
            }
        );

        let cache = &r.candidates[2].profile;
        assert_eq!((cache.engine, cache.port), (EngineKind::Valkey, 6380));
        assert_eq!(cache.auth, AuthMethod::None);
        assert!(cache.database.is_none());
    }

    #[test]
    fn missing_ssh_config_is_a_warning_not_a_failure() {
        let extra = Extra {
            datagrip_local: Some(LOCAL.into()),
            datagrip_ssh_configs: vec![],
        };
        let r = parse_text(ImportSource::DataGrip, DATA_SOURCES, "f", &extra);
        let prod = &r.candidates[0];
        assert!(prod.profile.tunnel.is_none());
        assert!(prod.warnings.iter().any(|w| w.contains("SSH tunnel")));
    }

    #[test]
    fn clipboard_snippet() {
        let snippet = "#DataSourceSettings#\n#LocalDataSource: pg@stage\n#BEGIN#\n<data-source source=\"LOCAL\" name=\"pg@stage\" uuid=\"x\"><driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://stage.db:5432/app</jdbc-url><secret-storage>master_key</secret-storage><user-name>dev</user-name><ssl-config><mode>REQUIRE</mode><enabled>true</enabled></ssl-config></data-source>\n#END#\n";
        let r = parse_text(
            ImportSource::DataGrip,
            snippet,
            "clipboard",
            &Extra::default(),
        );
        assert_eq!(r.candidates.len(), 1, "{r:?}");
        let p = &r.candidates[0].profile;
        assert_eq!(
            p.auth,
            AuthMethod::Password {
                username: "dev".into()
            }
        );
        assert_eq!(p.tls.mode, TlsMode::Require);
        assert_eq!(p.environment, Environment::Staging);
    }

    #[test]
    fn broken_xml_is_reported() {
        let r = parse_text(
            ImportSource::DataGrip,
            "<project><data-source name=\"x\"",
            "f",
            &Extra::default(),
        );
        assert!(r.error.is_some() || r.candidates.is_empty());
    }
}
