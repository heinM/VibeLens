//! Tool-call policy: a pure function from (call, connection facts) to a risk decision.
//!
//! Layers, innermost first: the backend runs agent reads in a READ ONLY transaction
//! (enforcement); this module routes each call to run / ask / refuse (policy); the real
//! boundary is a least-privilege database role (outside our control). SQL classification is
//! advisory but fail-closed: unparsed text is never read-only, and queries that call functions
//! with side effects (`dblink`, `pg_advisory_lock`, `pg_terminate_backend` …) need approval.
//!
//! Decisions, in one table (A = `aiDataAccess`):
//!
//! The SQL tools (`run_sql`, `get_schema`, `describe_table`) serve Postgres and Apache Iceberg
//! connections with identical access semantics; Iceberg writes (INSERT, CTAS, Athena DML) add a
//! note about the new table snapshot and, on Athena, the per-TB-scanned bill.
//!
//! | call | A = none | schemaOnly | resultsWithApproval | results |
//! |---|---|---|---|---|
//! | get_schema / describe_table / valkey_scan | denied | safe | safe | safe |
//! | run_sql, read-only | denied | **denied** | approval | safe |
//! | run_sql, session-only (SET, BEGIN …) | denied | denied | denied | denied |
//! | run_sql, anything else | denied | approval (rows withheld) | approval | approval |
//! | valkey_get / value-returning read | denied | denied | approval | safe |
//! | valkey metadata command (TTL, TYPE, INFO server/memory/keyspace/stats …) | denied | safe | safe | safe |
//! | valkey script/function (EVAL\*, FCALL\*, SCRIPT, FUNCTION) | denied | approval (result withheld) | approval | approval |
//! | valkey write | denied | approval (result withheld) | approval | approval |
//! | list_connections, list_files, read_file | safe | safe | safe | safe |
//! | write_file | approval | approval | approval | approval |
//!
//! MySQL connections get the Postgres semantics with MySQL's own classifier
//! (`mysql::classify`, `mysql::check_read_only`), and some statements are refused at every
//! access level, even with approval (`mysql::agent_refusal`: files on the server, server
//! configuration, KILL, plugins and loadable functions, USE, DELIMITER, executable comments).
//! The approval card names the host, port and user.
//!
//! Amazon Redshift connections get the Postgres semantics, with two differences: transaction
//! control (`BEGIN`, `COMMIT`, …) is refused outright, because the Data API commits each statement
//! on its own; and the approval card names the cluster or workgroup, database and region.

use super::tools::ToolCall;
use crate::model::{
    AiDataAccess, ApprovalTarget, ConnectionProfile, EngineKind, Environment, IcebergQueryEngine,
    RedshiftOptions, ToolRisk, effective_database,
};
use crate::sql::{SqlClassification, StatementClass};

/// The facts about a connection that policy needs. Never includes host or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileFacts {
    pub name: String,
    pub engine: EngineKind,
    pub environment: Environment,
    pub access: AiDataAccess,
    /// Iceberg connection whose SQL runs on Amazon Athena (billed per TB scanned, full DML)
    /// rather than the embedded engine (append-only). `false` for every other engine.
    pub athena: bool,
    /// Iceberg connection whose SQL runs on Trino or Spark: where, as
    /// `remote::config::engine_target` names it (engine, endpoint host and port, catalog; no
    /// credentials). Shown in approval text. `None` for every other engine.
    pub remote_engine: Option<String>,
    /// Iceberg connection on Trino or Spark: its engine (whose own read-only check corrects
    /// the PostgreSQL classifier, see `remote::classify_on`) and the approval card's detail
    /// line. `None` for every other engine.
    pub remote: Option<RemoteFacts>,
    /// The profile's configured database (Postgres and Redshift; `None` when unset or another
    /// engine). Always resolved through [`effective_database`], the rule execution uses.
    pub database: Option<String>,
    /// Redshift connection options (cluster or workgroup, region; identifiers, no secret), for
    /// the approval card's target line. `None` for every other engine.
    pub redshift: Option<RedshiftOptions>,
    /// MySQL connection: the approval card's detail line (`MySQL · host:port · as user`, from
    /// the profile). `None` for every other engine.
    pub mysql_detail: Option<String>,
}

/// See [`ProfileFacts::remote`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteFacts {
    pub engine: IcebergQueryEngine,
    pub detail: Option<String>,
}

impl From<&ConnectionProfile> for ProfileFacts {
    fn from(p: &ConnectionProfile) -> Self {
        Self {
            name: p.name.clone(),
            engine: p.engine,
            environment: p.environment,
            access: p.ai_data_access,
            athena: p.engine == EngineKind::Iceberg
                && p.iceberg
                    .as_ref()
                    .is_some_and(|o| matches!(o.query_engine, IcebergQueryEngine::Athena { .. })),
            remote_engine: p
                .iceberg
                .as_ref()
                .filter(|_| p.engine == EngineKind::Iceberg)
                .and_then(|o| crate::remote::config::engine_target(&o.query_engine)),
            remote: p
                .iceberg
                .as_ref()
                .filter(|o| {
                    p.engine == EngineKind::Iceberg
                        && matches!(
                            o.query_engine,
                            IcebergQueryEngine::Trino { .. } | IcebergQueryEngine::Spark { .. }
                        )
                })
                .map(|o| RemoteFacts {
                    engine: o.query_engine.clone(),
                    detail: crate::remote::config::engine_detail(o),
                }),
            database: match p.engine {
                EngineKind::Redshift => p.redshift.as_ref().map(|o| o.database.clone()),
                _ => p
                    .database
                    .clone()
                    .filter(|_| matches!(p.engine, EngineKind::Postgres | EngineKind::Mysql)),
            },
            redshift: p
                .redshift
                .clone()
                .filter(|_| p.engine == EngineKind::Redshift),
            mysql_detail: (p.engine == EngineKind::Mysql).then(|| crate::mysql::approval_detail(p)),
        }
    }
}

/// Where `call` will run, for the approval card's fixed header: the connection, its environment
/// and the Postgres database or Valkey db number. The model chooses the database and the db
/// number, so they are resolved here exactly as execution resolves them
/// ([`effective_database`]) and shown apart from anything the model wrote. `None` for tools
/// that touch no connection.
pub(super) fn approval_target(call: &ToolCall, f: &ProfileFacts) -> Option<ApprovalTarget> {
    call.profile_id()?;
    // Redshift resolves its database with the same rule (the profile's database is never empty).
    let postgres = has_databases(f.engine);
    let requested = match call {
        ToolCall::RunSql { database, .. }
        | ToolCall::GetSchema { database, .. }
        | ToolCall::DescribeTable { database, .. } => database.as_deref(),
        _ => None,
    };
    let valkey_db = match call {
        ToolCall::ValkeyScan { db, .. }
        | ToolCall::ValkeyGet { db, .. }
        | ToolCall::ValkeyCommand { db, .. } => Some(*db),
        _ => None,
    };
    Some(ApprovalTarget {
        connection: f.name.clone(),
        engine: f.engine,
        environment: f.environment,
        database: postgres.then(|| effective_database(f.database.as_deref(), requested)),
        default_database: postgres.then(|| effective_database(f.database.as_deref(), None)),
        valkey_db: valkey_db.filter(|_| f.engine == EngineKind::Valkey),
        // Redshift: cluster or workgroup, the database this call resolves to, region and who it
        // runs as. Trino and Spark: endpoint, catalog, namespace and acting user.
        detail: match &f.redshift {
            Some(o) => Some(crate::redshift::approval_detail(o, requested)),
            None => f
                .mysql_detail
                .clone()
                .or_else(|| f.remote.as_ref().and_then(|r| r.detail.clone())),
        },
    })
}

/// Engines whose connections hold several databases chosen with the `database` argument.
fn has_databases(e: EngineKind) -> bool {
    matches!(
        e,
        EngineKind::Postgres | EngineKind::Redshift | EngineKind::Mysql
    )
}

/// `'orders-db', database 'payments' (not this connection's default database 'shop')` for
/// Postgres, just the quoted connection name for Iceberg (which has no database to choose).
fn sql_target_text(f: &ProfileFacts, requested: Option<&str>) -> String {
    if let Some(remote) = &f.remote_engine {
        return format!("'{}' ({remote})", f.name);
    }
    if !has_databases(f.engine) {
        return format!("'{}'", f.name);
    }
    let database = effective_database(f.database.as_deref(), requested);
    let default = effective_database(f.database.as_deref(), None);
    if let Some(o) = &f.redshift {
        // `Redshift · {cluster or workgroup} · {database} · {region}`, from the profile.
        let target = crate::redshift::describe_target(o, &database);
        return if database == default {
            format!("'{}' ({target})", f.name)
        } else {
            format!(
                "'{}' ({target}; not this connection's default database '{default}')",
                f.name
            )
        };
    }
    if database == default {
        format!("'{}', database '{database}'", f.name)
    } else {
        format!(
            "'{}', database '{database}' (not this connection's default database '{default}')",
            f.name
        )
    }
}

/// The policy verdict plus how to execute if allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Assessment {
    pub risk: ToolRisk,
    /// Why (always set unless the call is plain `Safe`).
    pub reason: Option<String>,
    /// Text the approval card shows: the SQL, or the command line. (`write_file` previews are
    /// built by the caller because they need the existing file.)
    pub preview: Option<String>,
    /// Run with `read_only = false` / `allow_write = true`. Only ever set together with
    /// `NeedsApproval`.
    pub allow_write: bool,
    /// Withhold row values / command results from the model (approved writes on schema-only
    /// connections: `RETURNING` or `GETDEL` would otherwise leak data the user never agreed to share).
    pub redact_values: bool,
}

impl Assessment {
    fn safe() -> Self {
        Self {
            risk: ToolRisk::Safe,
            reason: None,
            preview: None,
            allow_write: false,
            redact_values: false,
        }
    }
    fn denied(reason: impl Into<String>) -> Self {
        Self {
            risk: ToolRisk::Denied,
            reason: Some(reason.into()),
            preview: None,
            allow_write: false,
            redact_values: false,
        }
    }
    fn approval(reason: impl Into<String>, preview: Option<String>) -> Self {
        Self {
            risk: ToolRisk::NeedsApproval,
            reason: Some(reason.into()),
            preview,
            allow_write: false,
            redact_values: false,
        }
    }
    fn write(mut self, redact: bool) -> Self {
        self.allow_write = true;
        self.redact_values = redact;
        self
    }
}

fn access_none(name: &str) -> Assessment {
    Assessment::denied(format!(
        "AI access to connection '{name}' is disabled (aiDataAccess = none). Do not use this connection; if the task needs it, tell the user they can change the setting in the connection's settings."
    ))
}

fn prod_prefix(f: &ProfileFacts) -> String {
    if f.environment == Environment::Prod {
        format!("PRODUCTION connection '{}': ", f.name)
    } else {
        String::new()
    }
}

/// Valkey commands that are never allowed from the agent, at any access level.
fn valkey_blocked(argv: &[String]) -> Option<&'static str> {
    let cmd = argv.first()?.to_ascii_uppercase();
    Some(match cmd.as_str() {
        "KEYS" => "KEYS blocks the server on large keyspaces; use valkey_scan instead",
        "CONFIG" | "ACL" | "DEBUG" | "MODULE" | "SLOWLOG" | "LATENCY" => {
            "server configuration, credentials and diagnostics are not available to the agent"
        }
        "CLIENT" | "CLUSTER" | "HELLO" | "AUTH" | "RESET" | "SELECT" | "QUIT" => {
            "connection-level commands are not available (the connection is shared); use the `db` argument instead of SELECT"
        }
        "MULTI" | "EXEC" | "DISCARD" | "WATCH" | "UNWATCH" => {
            "transactions span several commands and are not available"
        }
        "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "MONITOR" | "SYNC" | "PSYNC" => {
            "streaming commands would hang the connection"
        }
        // The list is the Valkey module's: the console refuses the same commands.
        _ if crate::valkey::is_blocking_command(argv) => {
            "blocking commands would hang the connection"
        }
        "SHUTDOWN" | "REPLICAOF" | "SLAVEOF" | "FAILOVER" | "MIGRATE" | "SAVE" | "BGSAVE"
        | "BGREWRITEAOF" => "server administration is not available to the agent",
        _ => return None,
    })
}

/// Commands that run arbitrary code: `COMMAND INFO` does not flag EVAL as a write, and the
/// `*_RO` variants are flagged read-only by the server yet can still run a Lua busy loop that
/// blocks the whole instance. All of them are forced through approval regardless of what the
/// server reports.
fn valkey_scripting(argv: &[String]) -> bool {
    let Some(cmd) = argv.first().map(|c| c.to_ascii_uppercase()) else {
        return false;
    };
    let base = cmd.strip_suffix("_RO").unwrap_or(&cmd);
    matches!(base, "EVAL" | "EVALSHA" | "FCALL" | "SCRIPT" | "FUNCTION")
}

/// `INFO` sections that describe the server's own state and nothing about its topology.
/// Everything else (`replication` carries primary/replica hosts and ports, `clients`,
/// `sentinel`, `cluster`, and the default/`all`/`everything` selections that include them)
/// is not metadata.
const INFO_SAFE_SECTIONS: &[&str] = &["server", "memory", "keyspace", "stats"];

/// `INFO` with explicit, individually safe sections. A bare `INFO` returns the default
/// selection, which includes `replication`, so it is not metadata.
fn valkey_info_is_metadata(argv: &[String]) -> bool {
    argv.len() > 1
        && argv[1..].iter().all(|section| {
            INFO_SAFE_SECTIONS
                .iter()
                .any(|s| section.eq_ignore_ascii_case(s))
        })
}

/// Read commands that return sizes, types and TTLs, never stored values.
fn valkey_metadata(argv: &[String]) -> bool {
    let up = |i: usize| argv.get(i).map(|s| s.to_ascii_uppercase());
    match up(0).as_deref() {
        Some(
            "TYPE" | "TTL" | "PTTL" | "EXISTS" | "SCAN" | "DBSIZE" | "STRLEN" | "LLEN" | "SCARD"
            | "HLEN" | "ZCARD" | "XLEN" | "EXPIRETIME" | "PEXPIRETIME" | "PING" | "TIME",
        ) => true,
        Some("INFO") => valkey_info_is_metadata(argv),
        Some("MEMORY") => up(1).as_deref() == Some("USAGE"),
        Some("OBJECT") => matches!(
            up(1).as_deref(),
            Some("ENCODING" | "IDLETIME" | "FREQ" | "REFCOUNT")
        ),
        _ => false,
    }
}

fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) || a.contains('"') {
                format!("{a:?}")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decides how a tool call is handled.
///
/// `profile` is the connection the call targets (`None` for workspace tools).
/// `valkey_write` is the backend's answer to "does this command write?" (`COMMAND INFO`
/// flags); callers must pass `true` when the backend could not tell (fail closed).
pub(super) fn assess(
    call: &ToolCall,
    profile: Option<&ProfileFacts>,
    valkey_write: bool,
) -> Assessment {
    // Workspace and catalogue tools do not touch a connection's data.
    match call {
        ToolCall::ListConnections | ToolCall::ListFiles | ToolCall::ReadFile { .. } => {
            return Assessment::safe();
        }
        ToolCall::WriteFile { path, .. } => {
            // The diff, and what it leaves out, are added where the file is read (session.rs).
            return Assessment::approval(
                format!(
                    "The agent wants to create or overwrite '{}' in your workspace. Review the diff before approving.",
                    super::diff::visible(path)
                ),
                None,
            );
        }
        _ => {}
    }
    let Some(f) = profile else {
        return Assessment::denied(
            "unknown connection: call list_connections and use one of the returned ids",
        );
    };
    if f.access == AiDataAccess::None {
        return access_none(&f.name);
    }
    // The SQL tools serve every SQL engine (Postgres, Iceberg, Redshift); list_databases only
    // those with databases to choose from; the rest are Valkey-only.
    if matches!(call, ToolCall::ListDatabases { .. }) && !has_databases(f.engine) {
        let hint = match f.engine {
            EngineKind::Iceberg => "it has namespaces, not databases: use get_schema to list them",
            _ => "use valkey_scan, valkey_get or valkey_command for it",
        };
        return Assessment::denied(format!(
            "list_databases works on Postgres, MySQL and Redshift connections; '{}' is not one: {hint}",
            f.name
        ));
    }
    let engine_fits = match call {
        ToolCall::ListDatabases { .. } => true,
        ToolCall::GetSchema { .. } | ToolCall::DescribeTable { .. } | ToolCall::RunSql { .. } => {
            matches!(
                f.engine,
                EngineKind::Postgres
                    | EngineKind::Iceberg
                    | EngineKind::Redshift
                    | EngineKind::Mysql
            )
        }
        _ => f.engine == EngineKind::Valkey,
    };
    if !engine_fits {
        let (have, hint) = match f.engine {
            EngineKind::Postgres => ("a Postgres", "use run_sql, get_schema or describe_table"),
            EngineKind::Valkey => ("a Valkey", "use valkey_scan, valkey_get or valkey_command"),
            EngineKind::Iceberg => ("an Iceberg", "use run_sql, get_schema or describe_table"),
            EngineKind::Redshift => ("a Redshift", "use run_sql, get_schema or describe_table"),
            EngineKind::Mysql => ("a MySQL", "use run_sql, get_schema or describe_table"),
        };
        return Assessment::denied(format!(
            "connection '{}' is {have} connection; {hint} for it",
            f.name
        ));
    }

    match call {
        // Names only, like the schema listing: structure, never data.
        ToolCall::ListDatabases { .. }
        | ToolCall::GetSchema { .. }
        | ToolCall::ValkeyScan { .. } => Assessment::safe(),
        // Column defaults and index predicates can hold literal values.
        ToolCall::DescribeTable { .. } => Assessment {
            redact_values: f.access != AiDataAccess::Results,
            ..Assessment::safe()
        },
        ToolCall::RunSql {
            database,
            sql,
            purpose,
            ..
        } => assess_sql(f, database.as_deref(), sql, purpose),
        ToolCall::ValkeyGet { db, key, .. } => match f.access {
            AiDataAccess::Results => Assessment::safe(),
            AiDataAccess::ResultsWithApproval => Assessment::approval(
                format!(
                    "Reading the value of '{key}' on '{}' (db {db}) sends stored data to the LLM provider; this connection requires approval for that.",
                    f.name
                ),
                Some(format!("GET {key}")),
            ),
            _ => Assessment::denied(format!(
                "connection '{}' is schema-only: values cannot be sent to the model. valkey_scan, TYPE, TTL and size commands still work; or ask the user to raise this connection's AI data access.",
                f.name
            )),
        },
        ToolCall::ValkeyCommand { db, argv, .. } => {
            assess_valkey_command(f, *db, argv, valkey_write)
        }
        // Handled above.
        ToolCall::ListConnections
        | ToolCall::ListFiles
        | ToolCall::ReadFile { .. }
        | ToolCall::WriteFile { .. } => Assessment::safe(),
    }
}

/// Redshift runs every statement of a script in its own Data API session, so `SET`/`RESET` lasts
/// only for itself: alone it does nothing, and in a script it does not apply to the statements
/// after it. Either way the call is denied with advice that works on Redshift.
fn redshift_session_statements(sql: &str) -> Option<Assessment> {
    let c = crate::sql::classify(sql);
    if !c.statements.contains(&StatementClass::Session) {
        return None;
    }
    Some(Assessment::denied(
        "on Redshift, SET and RESET last only for their own statement: the Data API runs each \
         statement of a script in its own session, so a setting never applies to the next \
         statement. Leave them out and qualify names instead (schema.table rather than a \
         search_path); there is no SET LOCAL or BEGIN … COMMIT to wrap them in.",
    ))
}

/// Extra approval-card text for Iceberg statements that change something.
fn iceberg_write_note(f: &ProfileFacts, c: &SqlClassification) -> String {
    if f.engine != EngineKind::Iceberg {
        return String::new();
    }
    let mut note = String::from(" Iceberg writes create a new table snapshot.");
    if f.athena {
        note.push_str(" Athena is billed per TB scanned.");
    } else if f.remote_engine.is_none() {
        // The embedded engine; Trino and Spark run full DML on Iceberg.
        let summary = c.summary.to_ascii_uppercase();
        if ["UPDATE", "DELETE", "MERGE"]
            .iter()
            .any(|kw| summary.contains(kw))
        {
            note.push_str(
                " The embedded Iceberg engine can only append (INSERT); UPDATE, DELETE and MERGE need an Athena connection and will fail here.",
            );
        }
    }
    note
}

/// `database` is what the model asked for; the reason names the database that results from it.
/// The target always comes before the statement summary and the model's `purpose`, so neither
/// can push it out of view. The preview is exactly the text that is executed.
fn assess_sql(f: &ProfileFacts, database: Option<&str>, sql: &str, purpose: &str) -> Assessment {
    // The engine refuses these too; refusing here tells the model why before the user is asked,
    // and keeps the classification below honest (a backslash in a literal means Redshift and the
    // classifier would read the statement differently).
    if f.engine == EngineKind::Redshift {
        if let Err(e) = crate::redshift::check_script(sql) {
            return Assessment::denied(e.to_string());
        }
        if let Some(denied) = redshift_session_statements(sql) {
            return denied;
        }
    }
    // MySQL: what reaches outside the database, or would read differently on the card than it
    // runs, is refused at every access level; everything else is classified by MySQL's rules.
    if f.engine == EngineKind::Mysql
        && let Some(why) = crate::mysql::agent_refusal(sql)
    {
        return Assessment::denied(format!(
            "{why}. This is not available to the agent on MySQL"
        ));
    }
    let (mut c, engine_refusal) = if f.engine == EngineKind::Mysql {
        (crate::mysql::classify(sql), None)
    } else {
        crate::remote::classify_on_explained(f.remote.as_ref().map(|r| &r.engine), sql)
    };
    // A statement the PostgreSQL classifier calls a read but the Trino or Spark read-only check
    // refuses (`java_method(…)`, `TABLE(pg.system.query(…))`) is refused outright with the
    // engine's reason. Asking for approval instead would run it with read_only = false, which
    // skips that very check.
    if let Some(reason) = engine_refusal {
        return Assessment::denied(format!(
            "{reason}. This connection's query engine refuses it in a read-only run, and the agent \
             does not run it as a write either"
        ));
    }
    // A Redshift query that calls a function that is not a read-only built-in is not run
    // read-only (the engine would refuse it): it is shown for approval like any unverified
    // statement.
    if f.engine == EngineKind::Redshift
        && c.overall == StatementClass::ReadOnly
        && let Some(func) = crate::redshift::unknown_function(sql)
    {
        c.overall = StatementClass::Unknown;
        c.summary = format!(
            "{} (calls {func}, not a read-only Redshift built-in)",
            c.summary
        );
    }
    let preview = Some(sql.to_string());
    let target = sql_target_text(f, database);
    match c.overall {
        StatementClass::ReadOnly => match f.access {
            AiDataAccess::Results => Assessment::safe(),
            AiDataAccess::ResultsWithApproval => Assessment::approval(
                format!(
                    "Read-only query on {target} ({}). The rows it returns will be sent to the LLM provider.{} Purpose: {purpose}",
                    c.summary,
                    if f.athena {
                        " Athena is billed per TB scanned."
                    } else {
                        ""
                    }
                ),
                preview,
            ),
            _ => Assessment::denied(format!(
                "connection '{}' is schema-only: query results cannot be sent to the model, so run_sql is refused (even for SELECT). Use get_schema or describe_table to inspect structure, \
                 or ask the user to raise this connection's AI data access to 'results' or 'resultsWithApproval'. You can also write the query to a .sql file with write_file for the user to run.",
                f.name
            )),
        },
        StatementClass::Session if c.statements.iter().all(|s| *s == StatementClass::Session) => {
            Assessment::denied(if f.engine == EngineKind::Iceberg {
                "session-only statements (SET, BEGIN, COMMIT, RESET …) have no lasting effect because each run_sql call uses its own session."
            } else if f.engine == EngineKind::Mysql {
                "session-only statements (SET, START TRANSACTION, COMMIT, LOCK TABLES …) have no lasting effect because each run_sql call uses its own session. \
             To change a setting for one query, send it in the same script, e.g. `SET SESSION sort_buffer_size = 1048576; SELECT …`."
            } else {
                "session-only statements (SET, BEGIN, COMMIT, RESET …) have no lasting effect because each run_sql call uses its own session. \
             To change a setting for one query, send it in the same script, e.g. `SET LOCAL statement_timeout = '5s'; SELECT …` inside BEGIN … COMMIT."
            })
        }
        class => {
            let prefix = prod_prefix(f);
            let what = match class {
                StatementClass::Session => "Session settings plus a query".to_string(),
                StatementClass::Write => "This changes data".to_string(),
                StatementClass::Ddl => {
                    "This changes the database schema or permissions".to_string()
                }
                _ => "This statement could not be verified as safe and may do anything".to_string(),
            };
            let parse_note = if c.parse_ok {
                ""
            } else {
                " (the SQL did not fully parse)"
            };
            let note = if class == StatementClass::Session {
                String::new()
            } else if f.engine == EngineKind::Redshift {
                " Redshift commits each statement on its own: a failure part-way leaves the earlier statements applied.".to_string()
            } else {
                iceberg_write_note(f, &c)
            };
            Assessment::approval(
                format!(
                    "{prefix}{what} on {target}: {}{parse_note}.{note} Purpose: {purpose}",
                    c.summary
                ),
                preview,
            )
            .write(f.access == AiDataAccess::SchemaOnly)
        }
    }
}

fn assess_valkey_command(
    f: &ProfileFacts,
    db: u8,
    argv: &[String],
    backend_says_write: bool,
) -> Assessment {
    if let Some(why) = valkey_blocked(argv) {
        return Assessment::denied(format!(
            "{} is not available: {why}.",
            argv[0].to_ascii_uppercase()
        ));
    }
    let preview = Some(command_line(argv));
    let is_write = backend_says_write || valkey_scripting(argv);
    if is_write {
        let prefix = prod_prefix(f);
        return Assessment::approval(
            format!(
                "{prefix}This command modifies data or runs code on '{}' (db {db}): {}",
                f.name,
                command_line(argv)
            ),
            preview,
        )
        .write(f.access == AiDataAccess::SchemaOnly);
    }
    if valkey_metadata(argv) {
        return Assessment::safe();
    }
    match f.access {
        AiDataAccess::Results => Assessment::safe(),
        AiDataAccess::ResultsWithApproval => Assessment::approval(
            format!(
                "Read command on '{}' (db {db}) returns stored values, which will be sent to the LLM provider: {}",
                f.name,
                command_line(argv)
            ),
            preview,
        ),
        _ => Assessment::denied(format!(
            "connection '{}' is schema-only: commands that return values are refused. Metadata commands (TYPE, TTL, EXISTS, SCAN, DBSIZE, INFO server|memory|keyspace|stats, STRLEN/LLEN/SCARD/HLEN/ZCARD, MEMORY USAGE) are allowed.",
            f.name
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AiDataAccess::{Results, ResultsWithApproval, SchemaOnly};
    use ToolRisk::*;

    fn pg(access: AiDataAccess, env: Environment) -> ProfileFacts {
        ProfileFacts {
            name: "orders-db".into(),
            engine: EngineKind::Postgres,
            environment: env,
            access,
            athena: false,
            remote_engine: None,
            remote: None,
            mysql_detail: None,
            database: Some("shop".into()),
            redshift: None,
        }
    }
    fn ice(access: AiDataAccess, env: Environment, athena: bool) -> ProfileFacts {
        ProfileFacts {
            name: "lake".into(),
            engine: EngineKind::Iceberg,
            environment: env,
            access,
            athena,
            remote_engine: None,
            remote: None,
            mysql_detail: None,
            database: None,
            redshift: None,
        }
    }
    fn vk(access: AiDataAccess, env: Environment) -> ProfileFacts {
        ProfileFacts {
            name: "cache".into(),
            engine: EngineKind::Valkey,
            environment: env,
            access,
            athena: false,
            remote_engine: None,
            remote: None,
            mysql_detail: None,
            database: None,
            redshift: None,
        }
    }
    fn rs(access: AiDataAccess, env: Environment) -> ProfileFacts {
        use crate::model::{AwsIdentity, AwsTarget, RedshiftAuth, RedshiftTarget};
        ProfileFacts {
            name: "warehouse".into(),
            engine: EngineKind::Redshift,
            environment: env,
            access,
            athena: false,
            remote_engine: None,
            remote: None,
            mysql_detail: None,
            database: Some("dev".into()),
            redshift: Some(RedshiftOptions {
                target: RedshiftTarget::Serverless {
                    workgroup_name: "analytics-wg".into(),
                },
                database: "dev".into(),
                aws: AwsTarget {
                    identity: AwsIdentity::DefaultChain,
                    region: "eu-west-1".into(),
                },
                auth: RedshiftAuth::IamIdentity,
            }),
        }
    }
    fn sql(s: &str) -> ToolCall {
        ToolCall::RunSql {
            profile_id: "p".into(),
            database: None,
            sql: s.into(),
            purpose: "testing".into(),
        }
    }
    fn vcmd(argv: &[&str]) -> ToolCall {
        ToolCall::ValkeyCommand {
            profile_id: "v".into(),
            db: 0,
            argv: argv.iter().map(|s| s.to_string()).collect(),
        }
    }
    const NOTHING: AiDataAccess = AiDataAccess::None;
    const ACCESS: [AiDataAccess; 4] = [NOTHING, SchemaOnly, ResultsWithApproval, Results];
    const ENVS: [Environment; 4] = [
        Environment::Dev,
        Environment::Test,
        Environment::Staging,
        Environment::Prod,
    ];

    #[test]
    fn access_none_denies_everything_that_touches_the_profile() {
        let calls = [
            ToolCall::GetSchema {
                profile_id: "p".into(),
                database: None,
                schema: None,
            },
            ToolCall::DescribeTable {
                profile_id: "p".into(),
                database: None,
                schema: Some("public".into()),
                table: "t".into(),
            },
            sql("select 1"),
            sql("drop table t"),
        ];
        for env in ENVS {
            for c in &calls {
                let a = assess(c, Some(&pg(NOTHING, env)), false);
                assert_eq!(a.risk, Denied, "{c:?}");
                assert!(a.reason.unwrap().contains("aiDataAccess = none"));
                assert!(!a.allow_write);
            }
            for c in [
                ToolCall::ValkeyScan {
                    profile_id: "v".into(),
                    db: 0,
                    pattern: "*".into(),
                    limit: 10,
                },
                ToolCall::ValkeyGet {
                    profile_id: "v".into(),
                    db: 0,
                    key: "k".into(),
                },
                vcmd(&["TTL", "k"]),
                vcmd(&["SET", "k", "v"]),
            ] {
                assert_eq!(
                    assess(&c, Some(&vk(NOTHING, env)), false).risk,
                    Denied,
                    "{c:?}"
                );
            }
        }
    }

    #[test]
    fn workspace_and_catalogue_tools_ignore_connection_policy() {
        for c in [
            ToolCall::ListConnections,
            ToolCall::ListFiles,
            ToolCall::ReadFile {
                path: "a.sql".into(),
            },
        ] {
            assert_eq!(assess(&c, None, false).risk, Safe);
        }
        let w = assess(
            &ToolCall::WriteFile {
                path: "a.sql".into(),
                contents: "x".into(),
            },
            None,
            false,
        );
        assert_eq!(w.risk, NeedsApproval);
        assert!(w.reason.unwrap().contains("a.sql"));
        assert!(!w.allow_write, "write_file never uses the SQL write flag");
    }

    #[test]
    fn schema_tools_are_safe_at_every_level_except_none() {
        for a in [SchemaOnly, ResultsWithApproval, Results] {
            assert_eq!(
                assess(
                    &ToolCall::GetSchema {
                        profile_id: "p".into(),
                        database: None,
                        schema: None
                    },
                    Some(&pg(a, Environment::Prod)),
                    false
                )
                .risk,
                Safe
            );
            assert_eq!(
                assess(
                    &ToolCall::DescribeTable {
                        profile_id: "p".into(),
                        database: None,
                        schema: Some("s".into()),
                        table: "t".into()
                    },
                    Some(&pg(a, Environment::Dev)),
                    false
                )
                .risk,
                Safe
            );
            assert_eq!(
                assess(
                    &ToolCall::ValkeyScan {
                        profile_id: "v".into(),
                        db: 0,
                        pattern: "*".into(),
                        limit: 5
                    },
                    Some(&vk(a, Environment::Prod)),
                    false
                )
                .risk,
                Safe
            );
        }
    }

    #[test]
    fn run_sql_read_only_matrix() {
        for env in ENVS {
            let a = assess(&sql("select * from t"), Some(&pg(SchemaOnly, env)), false);
            assert_eq!(a.risk, Denied);
            let reason = a.reason.unwrap();
            assert!(
                reason.contains("get_schema")
                    && reason.contains("describe_table")
                    && reason.contains("raise"),
                "{reason}"
            );

            let a = assess(
                &sql("select * from t"),
                Some(&pg(ResultsWithApproval, env)),
                false,
            );
            assert_eq!(a.risk, NeedsApproval);
            assert!(!a.allow_write, "approved reads still run read-only");
            assert_eq!(a.preview.as_deref(), Some("select * from t"));

            let a = assess(&sql("select * from t"), Some(&pg(Results, env)), false);
            assert_eq!(a.risk, Safe);
            assert!(!a.allow_write && a.reason.is_none());
        }
    }

    #[test]
    fn run_sql_non_reads_always_need_approval_and_run_read_write() {
        let scripts = [
            "insert into t values (1)",
            "update t set a = 1",
            "delete from t",
            "drop table t",
            "create index i on t(a)",
            "truncate t",
            "call p()",
            "do $$ begin end $$",
            "select * into x from t",
            "select * from t for update",
            "with d as (delete from t returning *) select * from d",
            "explain analyze delete from t",
            r#"explain ("analyze") delete from t"#,
            "explain delete from t",
            "select * from dblink('host=x', 'drop table t') as t(r text)",
            "select pg_advisory_lock(1)",
            "select pg_read_file('/etc/passwd')",
            "select lo_import('/etc/hostname')",
            "select set_config('transaction_read_only', 'off', false)",
            "select nextval('s')",
            "--x\rCOMMIT /*\nSHOW x */;\n--x\rDROP TABLE t /*\nSHOW x */",
            "select 1; drop table t",
            "gibberish ^^^",
            "begin; select 1; commit",
        ];
        for env in ENVS {
            for a in ACCESS {
                for s in scripts {
                    let r = assess(&sql(s), Some(&pg(a, env)), false);
                    if a == NOTHING {
                        assert_eq!(r.risk, Denied, "{s}");
                        continue;
                    }
                    assert_eq!(r.risk, NeedsApproval, "{s} @ {a:?}/{env:?}");
                    assert!(r.allow_write, "{s}");
                    assert_eq!(r.redact_values, a == SchemaOnly, "{s} @ {a:?}");
                    let reason = r.reason.unwrap();
                    assert_eq!(
                        reason.contains("PRODUCTION"),
                        env == Environment::Prod,
                        "{reason}"
                    );
                    assert!(reason.contains("testing"), "purpose is shown: {reason}");
                    assert_eq!(r.preview.as_deref(), Some(s));
                }
            }
        }
    }

    #[test]
    fn unparseable_unknown_is_flagged() {
        let r = assess(
            &sql("frobnicate ^^^"),
            Some(&pg(Results, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, NeedsApproval);
        assert!(r.reason.unwrap().contains("could not be verified"));
    }

    #[test]
    fn session_only_scripts_are_denied_but_mixed_ones_ask() {
        for a in [SchemaOnly, ResultsWithApproval, Results] {
            for s in [
                "set search_path = x",
                "begin",
                "commit",
                "begin; set local x = 1; commit",
                "reset all",
            ] {
                let r = assess(&sql(s), Some(&pg(a, Environment::Dev)), false);
                assert_eq!(r.risk, Denied, "{s}");
                assert!(r.reason.unwrap().contains("session-only"));
            }
            // Session statements wrapped around a query: approval, read-write.
            let r = assess(
                &sql("begin; select 1; commit"),
                Some(&pg(a, Environment::Dev)),
                false,
            );
            assert_eq!(r.risk, NeedsApproval);
            assert!(r.allow_write);
        }
    }

    #[test]
    fn empty_sql_is_safe_noop_class() {
        // Parsing guarantees non-empty SQL; a comment-only script classifies as read-only.
        assert_eq!(
            assess(
                &sql("-- nothing"),
                Some(&pg(Results, Environment::Dev)),
                false
            )
            .risk,
            Safe
        );
    }

    #[test]
    fn engine_mismatch_is_denied_with_guidance() {
        let r = assess(
            &sql("select 1"),
            Some(&vk(Results, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, Denied);
        assert!(r.reason.unwrap().contains("Valkey"));
        let r = assess(
            &vcmd(&["GET", "k"]),
            Some(&pg(Results, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, Denied);
        assert!(r.reason.unwrap().contains("run_sql"));
        let r = assess(
            &ToolCall::GetSchema {
                profile_id: "v".into(),
                database: None,
                schema: None,
            },
            Some(&vk(Results, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, Denied);
    }

    #[test]
    fn list_databases_is_safe_on_sql_servers_at_every_level_except_none() {
        let call = ToolCall::ListDatabases {
            profile_id: "p".into(),
        };
        for env in ENVS {
            for a in [SchemaOnly, ResultsWithApproval, Results] {
                for f in [pg(a, env), rs(a, env)] {
                    let v = assess(&call, Some(&f), false);
                    assert_eq!(v.risk, Safe, "{a:?} {env:?} {:?}", f.engine);
                    assert!(!v.allow_write && !v.redact_values);
                }
            }
            for f in [pg(NOTHING, env), rs(NOTHING, env)] {
                let v = assess(&call, Some(&f), false);
                assert_eq!(v.risk, Denied);
                assert!(v.reason.unwrap().contains("aiDataAccess = none"));
            }
        }
        let iceberg = assess(&call, Some(&ice(Results, Environment::Dev, false)), false);
        assert_eq!(iceberg.risk, Denied);
        assert!(iceberg.reason.unwrap().contains("get_schema"));
        let valkey = assess(&call, Some(&vk(Results, Environment::Dev)), false);
        assert_eq!(valkey.risk, Denied);
        assert!(valkey.reason.unwrap().contains("valkey_scan"));
        assert_eq!(
            assess(&call, None, false).risk,
            Denied,
            "unknown connection"
        );
    }

    #[test]
    fn missing_profile_is_denied() {
        let r = assess(&sql("select 1"), None, false);
        assert_eq!(r.risk, Denied);
        assert!(r.reason.unwrap().contains("list_connections"));
    }

    #[test]
    fn valkey_get_follows_access() {
        let get = ToolCall::ValkeyGet {
            profile_id: "v".into(),
            db: 0,
            key: "k".into(),
        };
        assert_eq!(
            assess(&get, Some(&vk(SchemaOnly, Environment::Dev)), false).risk,
            Denied
        );
        assert_eq!(
            assess(
                &get,
                Some(&vk(ResultsWithApproval, Environment::Dev)),
                false
            )
            .risk,
            NeedsApproval
        );
        assert_eq!(
            assess(&get, Some(&vk(Results, Environment::Dev)), false).risk,
            Safe
        );
        assert!(
            !assess(
                &get,
                Some(&vk(ResultsWithApproval, Environment::Dev)),
                false
            )
            .allow_write
        );
    }

    #[test]
    fn valkey_command_matrix() {
        for env in ENVS {
            // Writes: approval at every level, allow_write set, withheld results under schemaOnly.
            for a in [SchemaOnly, ResultsWithApproval, Results] {
                let r = assess(&vcmd(&["SET", "k", "v"]), Some(&vk(a, env)), true);
                assert_eq!(r.risk, NeedsApproval);
                assert!(r.allow_write);
                assert_eq!(r.redact_values, a == SchemaOnly);
                assert_eq!(
                    r.reason.unwrap().contains("PRODUCTION"),
                    env == Environment::Prod
                );
                assert_eq!(r.preview.as_deref(), Some("SET k v"));
            }
            // Value reads follow access.
            assert_eq!(
                assess(&vcmd(&["HGETALL", "h"]), Some(&vk(SchemaOnly, env)), false).risk,
                Denied
            );
            assert_eq!(
                assess(
                    &vcmd(&["HGETALL", "h"]),
                    Some(&vk(ResultsWithApproval, env)),
                    false
                )
                .risk,
                NeedsApproval
            );
            assert_eq!(
                assess(&vcmd(&["HGETALL", "h"]), Some(&vk(Results, env)), false).risk,
                Safe
            );
            // Metadata is safe at every non-none level.
            for argv in [
                &["TTL", "k"][..],
                &["type", "k"],
                &["EXISTS", "a", "b"],
                &["SCAN", "0"],
                &["DBSIZE"],
                &["INFO", "keyspace"],
                &["info", "Server", "MEMORY", "stats"],
                &["MEMORY", "USAGE", "k"],
                &["object", "encoding", "k"],
                &["LLEN", "l"],
            ] {
                for a in [SchemaOnly, ResultsWithApproval, Results] {
                    assert_eq!(
                        assess(&vcmd(argv), Some(&vk(a, env)), false).risk,
                        Safe,
                        "{argv:?}"
                    );
                }
            }
        }
        // MEMORY DOCTOR / OBJECT HELP are not metadata.
        assert_eq!(
            assess(
                &vcmd(&["MEMORY", "DOCTOR"]),
                Some(&vk(SchemaOnly, Environment::Dev)),
                false
            )
            .risk,
            Denied
        );
    }

    #[test]
    fn valkey_blocked_commands() {
        for argv in [
            &["KEYS", "*"][..],
            &["CONFIG", "GET", "requirepass"],
            &["ACL", "GETUSER", "default"],
            &["DEBUG", "SLEEP", "10"],
            &["CLIENT", "LIST"],
            &["SELECT", "1"],
            &["MULTI"],
            &["SUBSCRIBE", "c"],
            &["MONITOR"],
            &["BLPOP", "l", "0"],
            &["xread", "block", "0", "streams", "s", "$"],
            &["SHUTDOWN"],
            &["REPLICAOF", "h", "1"],
            &["CLUSTER", "NODES"],
            &["MIGRATE", "h", "1", "k", "0", "1"],
        ] {
            for a in [SchemaOnly, ResultsWithApproval, Results] {
                // Denied even if the backend (wrongly) says "write".
                for w in [false, true] {
                    let r = assess(&vcmd(argv), Some(&vk(a, Environment::Dev)), w);
                    assert_eq!(r.risk, Denied, "{argv:?}");
                    assert!(!r.allow_write);
                }
            }
        }
        // Non-blocking XREAD is a normal value read.
        assert_eq!(
            assess(
                &vcmd(&["XREAD", "STREAMS", "s", "0"]),
                Some(&vk(Results, Environment::Dev)),
                false
            )
            .risk,
            Safe
        );
        // KEYS hint points to valkey_scan.
        assert!(
            assess(
                &vcmd(&["KEYS", "*"]),
                Some(&vk(Results, Environment::Dev)),
                false
            )
            .reason
            .unwrap()
            .contains("valkey_scan")
        );
    }

    #[test]
    fn valkey_scripting_needs_approval_even_if_server_says_read() {
        for argv in [
            &["EVAL", "return 1", "0"][..],
            &["EVALSHA", "abc", "0"],
            &["FCALL", "f", "0"],
            &["SCRIPT", "FLUSH"],
            &["FUNCTION", "DELETE", "lib"],
        ] {
            let r = assess(&vcmd(argv), Some(&vk(Results, Environment::Dev)), false);
            assert_eq!(r.risk, NeedsApproval, "{argv:?}");
            assert!(r.allow_write);
        }
    }

    /// Reviewer finding: the server flags `EVAL_RO`/`FCALL_RO` as read-only commands, but a Lua
    /// busy loop still blocks the instance.
    #[test]
    fn valkey_read_only_script_variants_need_approval() {
        for argv in [
            &["EVAL_RO", "return 1", "0"][..],
            &["eval_ro", "while true do end", "0"],
            &["EVALSHA_RO", "abc", "0"],
            &["FCALL_RO", "f", "0"],
            &["fcall_ro", "f", "0"],
            &["SCRIPT", "EXISTS", "abc"],
            &["script", "load", "return 1"],
            &["FUNCTION", "LIST"],
            &["function", "stats"],
            &["FUNCTION", "DUMP"],
            &["EVAL", "return 1", "0"],
        ] {
            for a in [SchemaOnly, ResultsWithApproval, Results] {
                // The server says "read-only"; the policy must not believe it.
                let r = assess(&vcmd(argv), Some(&vk(a, Environment::Prod)), false);
                assert_eq!(r.risk, NeedsApproval, "{argv:?} @ {a:?}");
                assert!(r.allow_write, "{argv:?}");
                assert_eq!(r.redact_values, a == SchemaOnly, "{argv:?}");
                assert!(r.reason.unwrap().contains("PRODUCTION"));
            }
        }
        assert!(!valkey_scripting(&[]));
        assert!(!valkey_scripting(&["GET".into(), "EVAL_RO".into()]));
        assert!(!valkey_scripting(&["EVALX".into()]));
    }

    /// Reviewer finding: `INFO` under schemaOnly exposed replication hosts.
    #[test]
    fn valkey_info_is_metadata_only_for_safe_sections() {
        for argv in [
            &["INFO"][..],
            &["INFO", "replication"],
            &["info", "REPLICATION"],
            &["INFO", "clients"],
            &["INFO", "sentinel"],
            &["INFO", "cluster"],
            &["INFO", "all"],
            &["INFO", "everything"],
            &["INFO", "default"],
            &["INFO", "commandstats"],
            &["INFO", "keyspace", "replication"],
            &["INFO", "server", "bogus"],
        ] {
            assert_eq!(
                assess(&vcmd(argv), Some(&vk(SchemaOnly, Environment::Dev)), false).risk,
                Denied,
                "{argv:?} must not be readable under schemaOnly"
            );
            assert_eq!(
                assess(
                    &vcmd(argv),
                    Some(&vk(ResultsWithApproval, Environment::Dev)),
                    false
                )
                .risk,
                NeedsApproval,
                "{argv:?}"
            );
            assert_eq!(
                assess(&vcmd(argv), Some(&vk(Results, Environment::Dev)), false).risk,
                Safe,
                "{argv:?}"
            );
        }
        for argv in [
            &["INFO", "server"][..],
            &["INFO", "memory"],
            &["INFO", "KEYSPACE"],
            &["INFO", "stats"],
            &["INFO", "server", "memory", "keyspace", "stats"],
        ] {
            assert_eq!(
                assess(&vcmd(argv), Some(&vk(SchemaOnly, Environment::Dev)), false).risk,
                Safe,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn command_line_quotes_awkward_args() {
        let argv: Vec<String> = ["SET", "my key", "", "q\"x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(command_line(&argv), "SET \"my key\" \"\" \"q\\\"x\"");
    }

    // ---- Apache Iceberg ------------------------------------------------------------------

    fn describe(profile: &str) -> ToolCall {
        ToolCall::DescribeTable {
            profile_id: profile.into(),
            database: None,
            schema: Some("shop".into()),
            table: "orders".into(),
        }
    }

    #[test]
    fn iceberg_sql_tools_are_accepted_and_valkey_tools_are_not() {
        for athena in [false, true] {
            let f = ice(Results, Environment::Dev, athena);
            for c in [
                ToolCall::GetSchema {
                    profile_id: "i".into(),
                    database: None,
                    schema: Some("shop".into()),
                },
                describe("i"),
                sql("select 1"),
            ] {
                assert_eq!(assess(&c, Some(&f), false).risk, Safe, "{c:?}");
            }
            for c in [
                vcmd(&["GET", "k"]),
                ToolCall::ValkeyScan {
                    profile_id: "i".into(),
                    db: 0,
                    pattern: "*".into(),
                    limit: 5,
                },
            ] {
                let r = assess(&c, Some(&f), false);
                assert_eq!(r.risk, Denied, "{c:?}");
                let reason = r.reason.unwrap();
                assert!(
                    reason.contains("Iceberg") && reason.contains("run_sql"),
                    "{reason}"
                );
            }
        }
        // And a Valkey connection still cannot take the SQL tools.
        let r = assess(
            &sql("select 1"),
            Some(&vk(Results, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, Denied);
        assert!(r.reason.unwrap().contains("valkey_scan"));
    }

    #[test]
    fn iceberg_access_none_denies_every_sql_tool() {
        for athena in [false, true] {
            for env in ENVS {
                let f = ice(NOTHING, env, athena);
                for c in [
                    sql("select 1"),
                    sql("insert into t values (1)"),
                    describe("i"),
                ] {
                    let r = assess(&c, Some(&f), false);
                    assert_eq!(r.risk, Denied, "{c:?}");
                    assert!(r.reason.unwrap().contains("aiDataAccess = none"));
                }
            }
        }
    }

    #[test]
    fn iceberg_read_matrix_matches_postgres() {
        let reads = [
            "select * from shop.customers limit 10",
            "select count(*) from shop.orders",
            r#"select * from shop."orders@123456789" limit 5"#,
            "with c as (select * from shop.customers) select * from c",
            "explain select * from shop.customers",
        ];
        for athena in [false, true] {
            for env in ENVS {
                for s in reads {
                    let r = assess(&sql(s), Some(&ice(SchemaOnly, env, athena)), false);
                    assert_eq!(r.risk, Denied, "{s}");
                    assert!(r.reason.unwrap().contains("schema-only"));

                    let r = assess(&sql(s), Some(&ice(ResultsWithApproval, env, athena)), false);
                    assert_eq!(r.risk, NeedsApproval, "{s}");
                    assert!(!r.allow_write, "approved reads still run read-only: {s}");
                    let reason = r.reason.unwrap();
                    assert_eq!(reason.contains("billed per TB"), athena, "{reason}");
                    assert_eq!(r.preview.as_deref(), Some(s));

                    let r = assess(&sql(s), Some(&ice(Results, env, athena)), false);
                    assert_eq!(r.risk, Safe, "{s}");
                    assert!(!r.allow_write && r.reason.is_none());
                }
            }
        }
    }

    #[test]
    fn iceberg_writes_need_approval_and_mention_the_snapshot() {
        let writes = [
            "insert into shop.customers values (1, 'a')",
            "insert into shop.orders select * from shop.staging",
            "create table shop.top as select * from shop.orders",
            "update shop.orders set status = 'x' where id = 1",
            "delete from shop.orders where id = 1",
            "merge into shop.orders t using shop.stg s on t.id = s.id when matched then update set status = s.status",
            "drop table shop.orders",
            "alter table shop.orders add column note varchar",
        ];
        for athena in [false, true] {
            for env in ENVS {
                for a in [SchemaOnly, ResultsWithApproval, Results] {
                    for s in writes {
                        let r = assess(&sql(s), Some(&ice(a, env, athena)), false);
                        assert_eq!(r.risk, NeedsApproval, "{s}");
                        assert!(r.allow_write, "{s}");
                        assert_eq!(r.redact_values, a == SchemaOnly, "{s}");
                        let reason = r.reason.unwrap();
                        assert!(
                            reason.contains("Iceberg writes create a new table snapshot"),
                            "{reason}"
                        );
                        assert_eq!(reason.contains("billed per TB scanned"), athena, "{reason}");
                        assert_eq!(
                            reason.contains("PRODUCTION connection 'lake'"),
                            env == Environment::Prod,
                            "{reason}"
                        );
                        assert!(reason.contains("testing"), "{reason}");
                        assert_eq!(r.preview.as_deref(), Some(s));
                    }
                }
            }
        }
    }

    #[test]
    fn iceberg_row_level_dml_on_the_local_engine_points_to_athena() {
        for s in [
            "update shop.orders set status = 'x'",
            "delete from shop.orders",
            "merge into shop.orders t using shop.s s on t.id = s.id when matched then delete",
        ] {
            let local = assess(&sql(s), Some(&ice(Results, Environment::Dev, false)), false);
            assert_eq!(local.risk, NeedsApproval, "{s}");
            let reason = local.reason.unwrap();
            assert!(
                reason.contains("need an Athena connection") && !reason.contains("billed"),
                "{reason}"
            );

            let athena = assess(&sql(s), Some(&ice(Results, Environment::Dev, true)), false);
            assert_eq!(athena.risk, NeedsApproval, "{s}");
            let reason = athena.reason.unwrap();
            assert!(
                reason.contains("billed per TB scanned") && !reason.contains("need an Athena"),
                "{reason}"
            );
        }
        // An append is fine locally: no limitation note.
        let r = assess(
            &sql("insert into shop.customers values (1, 'a')"),
            Some(&ice(Results, Environment::Dev, false)),
            false,
        );
        assert!(!r.reason.unwrap().contains("Athena"));
    }

    #[test]
    fn iceberg_unverifiable_sql_fails_closed() {
        // Dialect features sqlparser's Postgres grammar rejects must never run unasked.
        for s in [
            "optimize shop.orders rewrite data using bin_pack",
            "vacuum shop.orders",
            "unload (select * from shop.orders) to 's3://b/p/' with (format = 'PARQUET')",
            "select * from shop.orders for version as of 123",
            "frobnicate ^^^",
            "select 1; drop table shop.orders",
            "create external table x stored as parquet location '/etc'",
            "copy (select * from shop.orders) to '/tmp/x.parquet'",
        ] {
            for athena in [false, true] {
                let r = assess(
                    &sql(s),
                    Some(&ice(Results, Environment::Prod, athena)),
                    false,
                );
                assert_eq!(r.risk, NeedsApproval, "{s}");
                assert!(r.allow_write, "{s}");
                let reason = r.reason.unwrap();
                assert!(reason.contains("PRODUCTION"), "{reason}");
            }
        }
    }

    #[test]
    fn iceberg_session_only_statements_are_denied_without_postgres_advice() {
        let r = assess(
            &sql("set x = 1"),
            Some(&ice(Results, Environment::Dev, false)),
            false,
        );
        assert_eq!(r.risk, Denied);
        let reason = r.reason.unwrap();
        assert!(reason.contains("session-only") && !reason.contains("statement_timeout"));
    }

    #[test]
    fn facts_detect_the_athena_engine() {
        use crate::model::{IcebergCatalog, IcebergOptions, IcebergStorage};
        let mut p = ConnectionProfile {
            id: "i".into(),
            name: "lake".into(),
            engine: EngineKind::Iceberg,
            host: String::new(),
            port: 0,
            database: None,
            auth: crate::model::AuthMethod::None,
            tls: Default::default(),
            tunnel: None,
            environment: Environment::Dev,
            ai_data_access: Results,
            color: None,
            pg: Default::default(),
            valkey: Default::default(),
            created_at: None,
            updated_at: None,
            iceberg: Some(IcebergOptions {
                catalog: IcebergCatalog::Sql {
                    uri: "sqlite:///x.db".into(),
                    warehouse: "/w".into(),
                    catalog_name: "c".into(),
                },
                storage: IcebergStorage::Local,
                query_engine: IcebergQueryEngine::Local,
                default_namespace: None,
            }),
            redshift: None,
            mysql: Default::default(),
        };
        assert!(!ProfileFacts::from(&p).athena);
        p.iceberg.as_mut().unwrap().query_engine = IcebergQueryEngine::Athena {
            workgroup: "primary".into(),
            output_location: None,
            aws: None,
        };
        let f = ProfileFacts::from(&p);
        assert!(f.athena && f.engine == EngineKind::Iceberg);
        // The facts never carry catalog details.
        assert!(!format!("{f:?}").contains("sqlite"));
        assert_eq!(f.remote_engine, None);

        // Trino and Spark: the approval text names engine, endpoint host and catalog (never a
        // credential), has no per-TB note, and does not send the user to Athena for DML.
        p.iceberg.as_mut().unwrap().query_engine = IcebergQueryEngine::Trino {
            url: "https://trino.example:8443/".into(),
            catalog: "lake".into(),
            user: "ana".into(),
            auth: crate::model::HttpEngineAuth::Basic {
                username: "ana".into(),
            },
            ca_cert_path: None,
        };
        let f = ProfileFacts::from(&p);
        assert!(!f.athena);
        assert_eq!(
            f.remote_engine.as_deref(),
            Some("Trino at trino.example:8443, catalog 'lake'")
        );
        for s in ["delete from shop.t where id = 1", "select 1"] {
            let mut g = f.clone();
            g.access = ResultsWithApproval;
            let r = assess(&sql(s), Some(&g), false).reason.unwrap();
            assert!(
                r.contains("'lake' (Trino at trino.example:8443, catalog 'lake')")
                    && !r.contains("billed")
                    && !r.contains("need an Athena")
                    && !r.contains("ana"),
                "{r}"
            );
        }
        p.iceberg.as_mut().unwrap().query_engine = IcebergQueryEngine::Spark {
            url: "https://livy.example".into(),
            catalog: "glue".into(),
            auth: crate::model::HttpEngineAuth::None,
            ca_cert_path: None,
            proxy_user: None,
            session_conf: Default::default(),
            session_start_timeout_secs: None,
        };
        assert_eq!(
            ProfileFacts::from(&p).remote_engine.as_deref(),
            Some("Spark (Livy) at livy.example, catalog 'glue'")
        );

        // Audit 2026-11-21, item 14: the approval card's detail line names engine, endpoint,
        // catalog and namespace, never a credential.
        p.iceberg.as_mut().unwrap().default_namespace = Some("shop".into());
        let mut f = ProfileFacts::from(&p);
        let t = approval_target(&sql("select 1"), &f).unwrap();
        assert_eq!(
            t.detail.as_deref(),
            Some("Spark (Livy) · livy.example:443 · catalog glue · namespace shop")
        );

        // Item 13: Spark SQL the PostgreSQL parser cannot read is judged by the engine's own
        // check. Without it an ordinary read showed a "may do anything" card and, once
        // approved, ran with read_only = false, skipping the Spark scan.
        f.access = Results;
        for s in [
            "SELECT `id` FROM lake.shop.orders VERSION AS OF 1 LIMIT 10",
            "SHOW TABLES IN lake.shop",
        ] {
            let r = assess(&sql(s), Some(&f), false);
            assert_eq!(r.risk, Safe, "{s}: {r:?}");
        }
        for s in [
            // The PostgreSQL classifier calls this a read: it runs read-only, where the
            // Spark check refuses it before submission (remote_spark tests).
            "SELECT reflect('java.lang.Runtime', 'getRuntime')",
            // Unknown to both: approval, as before.
            "SELECT * FROM text.`/etc/passwd`",
        ] {
            let r = assess(&sql(s), Some(&f), false);
            assert!(r.risk != Safe || !r.allow_write, "{s}: {r:?}");
        }
        // Re-audit item 6: a PostgreSQL "read" the Spark check refuses is refused outright with
        // the engine's reason; approving it would run it with read_only = false and skip the
        // check.
        for s in [
            "SELECT java_method('java.lang.Runtime', 'getRuntime')",
            "SELECT reflect('java.lang.Runtime', 'getRuntime')",
        ] {
            let r = assess(&sql(s), Some(&f), false);
            assert_eq!(r.risk, Denied, "{s}: {r:?}");
            assert!(r.reason.unwrap().contains("read-only run"), "{s}");
        }
        for s in [
            "DELETE FROM lake.shop.orders",
            "INSERT INTO t SELECT * FROM `x`",
        ] {
            let r = assess(&sql(s), Some(&f), false);
            assert_eq!(r.risk, NeedsApproval, "{s}");
        }
        // Never the other way round: a write the PostgreSQL classifier sees stays a write.
        let r = assess(&sql("DELETE FROM t"), Some(&f), false);
        assert!(r.allow_write && r.risk == NeedsApproval);

        // Trino: same, with Athena's check.
        p.iceberg.as_mut().unwrap().query_engine = IcebergQueryEngine::Trino {
            url: "https://trino.example/".into(),
            catalog: "lake".into(),
            user: "ana".into(),
            auth: crate::model::HttpEngineAuth::None,
            ca_cert_path: None,
        };
        let mut f = ProfileFacts::from(&p);
        f.access = Results;
        let r = assess(
            &sql("SELECT * FROM shop.orders FOR VERSION AS OF 1 LIMIT 5"),
            Some(&f),
            false,
        );
        assert_eq!(r.risk, Safe, "{r:?}");
    }

    // ---- The target the user approves (review finding M3) --------------------------------

    fn sql_on(database: Option<&str>, s: &str) -> ToolCall {
        ToolCall::RunSql {
            profile_id: "p".into(),
            database: database.map(String::from),
            sql: s.into(),
            purpose: "testing".into(),
        }
    }

    /// The model chooses the database, so the approval text must name the one that will be used.
    #[test]
    fn sql_approval_reason_names_the_database_the_statement_runs_against() {
        for (access, s) in [
            (Results, "delete from t"),
            (ResultsWithApproval, "select * from t"),
        ] {
            let r = assess(
                &sql_on(Some("payments_prod"), s),
                Some(&pg(access, Environment::Dev)),
                false,
            );
            assert_eq!(r.risk, NeedsApproval, "{s}");
            let reason = r.reason.unwrap();
            assert!(reason.contains("payments_prod"), "{reason}");
        }
    }

    #[test]
    fn sql_approval_reason_says_when_the_database_is_not_the_default() {
        let facts = pg(Results, Environment::Prod);
        let reason = |database: Option<&str>| {
            assess(&sql_on(database, "delete from t"), Some(&facts), false)
                .reason
                .unwrap()
        };
        // Named or not, the connection's own database is shown without a warning.
        for same in [None, Some("shop"), Some("  "), Some(" shop ")] {
            let r = reason(same);
            assert!(r.contains("on 'orders-db', database 'shop':"), "{r}");
            assert!(!r.contains("not this connection's default"), "{r}");
        }
        let r = reason(Some("payments_prod"));
        assert!(
            r.contains(
                "on 'orders-db', database 'payments_prod' (not this connection's default database 'shop'):"
            ),
            "{r}"
        );
        // The target comes before the statement summary and the model's purpose.
        let (target_at, summary_at, purpose_at) = (
            r.find("payments_prod").unwrap(),
            r.find("1 statement").unwrap(),
            r.find("Purpose:").unwrap(),
        );
        assert!(target_at < summary_at && summary_at < purpose_at, "{r}");

        // A profile with no database set runs on `postgres`, like the connection manager.
        let unset = ProfileFacts {
            database: None,
            ..pg(Results, Environment::Dev)
        };
        let r = assess(&sql_on(None, "delete from t"), Some(&unset), false)
            .reason
            .unwrap();
        assert!(r.contains("database 'postgres'"), "{r}");
        // Iceberg has no database to choose; the argument is ignored there.
        let r = assess(
            &sql_on(Some("ignored"), "insert into shop.t values (1)"),
            Some(&ice(Results, Environment::Dev, false)),
            false,
        )
        .reason
        .unwrap();
        assert!(r.contains("on 'lake':") && !r.contains("ignored"), "{r}");
    }

    #[test]
    fn approval_target_resolves_the_database_like_execution() {
        let facts = pg(Results, Environment::Staging);
        let t = approval_target(&sql_on(Some("payments_prod"), "select 1"), &facts).unwrap();
        assert_eq!(
            t,
            ApprovalTarget {
                connection: "orders-db".into(),
                engine: EngineKind::Postgres,
                environment: Environment::Staging,
                database: Some("payments_prod".into()),
                default_database: Some("shop".into()),
                valkey_db: None,
                detail: None,
            }
        );
        let t = approval_target(&sql_on(None, "select 1"), &facts).unwrap();
        assert_eq!(t.database.as_deref(), Some("shop"));
        assert_eq!(t.database, t.default_database);

        // Same rule as `ConnectionProfile::effective_database`, for every combination.
        for configured in [None, Some("shop"), Some(""), Some("  ")] {
            for requested in [None, Some("other"), Some(""), Some(" other ")] {
                let facts = ProfileFacts {
                    database: configured.map(String::from),
                    ..pg(Results, Environment::Dev)
                };
                let t = approval_target(&sql_on(requested, "select 1"), &facts).unwrap();
                assert_eq!(
                    t.database,
                    Some(effective_database(configured, requested)),
                    "{configured:?} / {requested:?}"
                );
            }
        }

        let v = approval_target(
            &ToolCall::ValkeyCommand {
                profile_id: "v".into(),
                db: 3,
                argv: vec!["DEL".into(), "k".into()],
            },
            &vk(Results, Environment::Prod),
        )
        .unwrap();
        assert_eq!((v.valkey_db, v.database.as_deref()), (Some(3), None));
        assert_eq!(v.environment, Environment::Prod);

        let i = approval_target(
            &sql_on(Some("ignored"), "select 1"),
            &ice(Results, Environment::Dev, true),
        )
        .unwrap();
        assert_eq!(
            (i.database, i.default_database, i.valkey_db),
            (None, None, None)
        );

        // Workspace tools touch no connection.
        let w = ToolCall::WriteFile {
            path: "a.sql".into(),
            contents: "x".into(),
        };
        assert!(approval_target(&w, &facts).is_none());
    }

    #[test]
    fn facts_carry_the_configured_database_for_postgres_only() {
        let mut p = ConnectionProfile {
            id: "p".into(),
            name: "orders-db".into(),
            engine: EngineKind::Postgres,
            host: "h".into(),
            port: 5432,
            database: Some("shop".into()),
            auth: crate::model::AuthMethod::None,
            tls: Default::default(),
            tunnel: None,
            environment: Environment::Dev,
            ai_data_access: Results,
            color: None,
            pg: Default::default(),
            valkey: Default::default(),
            created_at: None,
            updated_at: None,
            iceberg: None,
            redshift: None,
            mysql: Default::default(),
        };
        assert_eq!(ProfileFacts::from(&p).database.as_deref(), Some("shop"));
        p.engine = EngineKind::Valkey;
        assert_eq!(ProfileFacts::from(&p).database, None);
    }

    #[test]
    fn valkey_approval_reason_names_the_logical_database() {
        let on_db3 = |argv: &[&str]| ToolCall::ValkeyCommand {
            profile_id: "v".into(),
            db: 3,
            argv: argv.iter().map(|s| s.to_string()).collect(),
        };
        let write = assess(
            &on_db3(&["DEL", "k"]),
            Some(&vk(Results, Environment::Dev)),
            true,
        );
        assert!(
            write.reason.as_deref().unwrap().contains("db 3"),
            "{write:?}"
        );
        let read = assess(
            &on_db3(&["HGETALL", "h"]),
            Some(&vk(ResultsWithApproval, Environment::Dev)),
            false,
        );
        assert!(read.reason.as_deref().unwrap().contains("db 3"), "{read:?}");
        let get = assess(
            &ToolCall::ValkeyGet {
                profile_id: "v".into(),
                db: 3,
                key: "k".into(),
            },
            Some(&vk(ResultsWithApproval, Environment::Dev)),
            false,
        );
        assert!(get.reason.as_deref().unwrap().contains("db 3"), "{get:?}");
    }

    #[test]
    fn allow_write_implies_needs_approval_everywhere() {
        let calls: Vec<ToolCall> = vec![
            sql("select 1"),
            sql("update t set a=1"),
            sql("set x=1"),
            vcmd(&["GET", "k"]),
            vcmd(&["SET", "k", "v"]),
            vcmd(&["EVAL", "x", "0"]),
            vcmd(&["KEYS", "*"]),
        ];
        for a in ACCESS {
            for env in ENVS {
                for c in &calls {
                    for w in [false, true] {
                        let facts = if c.name() == "run_sql" {
                            pg(a, env)
                        } else {
                            vk(a, env)
                        };
                        let r = assess(c, Some(&facts), w);
                        if r.allow_write {
                            assert_eq!(r.risk, NeedsApproval, "{c:?}");
                        }
                        if r.risk == Safe {
                            assert!(r.reason.is_none() && !r.allow_write);
                        } else {
                            assert!(r.reason.is_some());
                        }
                    }
                }
            }
        }
    }

    // ---- Amazon Redshift -----------------------------------------------------------------

    #[test]
    fn redshift_uses_the_sql_tools_with_postgres_access_semantics() {
        let get = ToolCall::GetSchema {
            profile_id: "p".into(),
            database: None,
            schema: None,
        };
        for env in ENVS {
            assert_eq!(assess(&get, Some(&rs(SchemaOnly, env)), false).risk, Safe);
            assert_eq!(assess(&get, Some(&rs(NOTHING, env)), false).risk, Denied);
            let q = sql("select * from sales limit 10");
            assert_eq!(assess(&q, Some(&rs(SchemaOnly, env)), false).risk, Denied);
            assert_eq!(
                assess(&q, Some(&rs(ResultsWithApproval, env)), false).risk,
                NeedsApproval
            );
            assert_eq!(assess(&q, Some(&rs(Results, env)), false).risk, Safe);
            let w = assess(&sql("delete from sales"), Some(&rs(SchemaOnly, env)), false);
            assert_eq!(w.risk, NeedsApproval);
            assert!(w.allow_write && w.redact_values);
            assert!(
                w.reason
                    .unwrap()
                    .contains("commits each statement on its own"),
                "a write says it is not transactional"
            );
        }
        let v = assess(
            &vcmd(&["GET", "k"]),
            Some(&rs(Results, Environment::Dev)),
            false,
        );
        assert_eq!(v.risk, Denied);
        assert!(v.reason.unwrap().contains("a Redshift connection"));
    }

    #[test]
    fn redshift_refuses_backslash_literals_before_anyone_is_asked() {
        // Audit HIGH 1: the classifier reads these as plain SELECTs of two literals; Redshift
        // runs `SELECT … INTO public.pwned` and `pg_terminate_backend(123)`.
        for s in [
            r"SELECT '\' , ' AS x INTO public.pwned FROM secrets -- '",
            r"SELECT '\' , ' , pg_terminate_backend(123) -- '",
        ] {
            for a in [SchemaOnly, ResultsWithApproval, Results] {
                for env in ENVS {
                    let r = assess(&sql(s), Some(&rs(a, env)), false);
                    assert_eq!(r.risk, Denied, "{s}");
                    assert!(r.reason.unwrap().contains("backslash"), "{s}");
                }
            }
            // On Postgres (standard strings) the same text stays a plain query.
            assert_eq!(
                assess(&sql(s), Some(&pg(Results, Environment::Dev)), false).risk,
                Safe
            );
        }
    }

    #[test]
    fn redshift_unknown_functions_need_approval_instead_of_running_read_only() {
        for s in [
            "SELECT f_notify(card_number) FROM payments",
            "SELECT public.f(x) FROM t",
            "SELECT change_query_priority(1, 'high')",
        ] {
            let r = assess(&sql(s), Some(&rs(Results, Environment::Dev)), false);
            assert_eq!(r.risk, NeedsApproval, "{s}");
            assert!(
                r.allow_write,
                "approved, it runs without the read-only check"
            );
            // The admin functions are on the general deny-list, so the classifier names them.
            let m = r.reason.unwrap();
            assert!(
                m.contains("not a read-only Redshift built-in")
                    || m.contains("change_query_priority()"),
                "{m}"
            );
        }
        let r = assess(
            &sql("SELECT lower(name), count(*) FROM t GROUP BY 1"),
            Some(&rs(Results, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, Safe);
    }

    #[test]
    fn redshift_session_statements_get_redshift_advice() {
        for s in [
            "SET search_path TO x",
            "SET search_path TO x; SELECT * FROM t",
            "SELECT 1; RESET search_path",
        ] {
            let r = assess(&sql(s), Some(&rs(Results, Environment::Dev)), false);
            assert_eq!(r.risk, Denied, "{s}");
            let m = r.reason.unwrap();
            assert!(
                m.contains("qualify names") && !m.contains("SET LOCAL statement_timeout"),
                "{m}"
            );
        }
    }

    #[test]
    fn redshift_refuses_transaction_control_at_every_access_level() {
        for a in [SchemaOnly, ResultsWithApproval, Results] {
            for s in [
                "BEGIN",
                "begin; delete from t; commit",
                "START TRANSACTION",
                "delete from t; ROLLBACK",
                "END",
                "ABORT",
            ] {
                let r = assess(&sql(s), Some(&rs(a, Environment::Dev)), false);
                assert_eq!(r.risk, Denied, "{s}");
                assert!(
                    r.reason
                        .unwrap()
                        .contains("commits each statement on its own"),
                    "{s}"
                );
            }
        }
    }

    #[test]
    fn redshift_approval_names_workgroup_database_and_region() {
        let f = rs(ResultsWithApproval, Environment::Prod);
        let r = assess(&sql_on(None, "select 1"), Some(&f), false)
            .reason
            .unwrap();
        assert!(
            r.contains(
                "'warehouse' (Redshift · analytics-wg · dev · eu-west-1 · as the IAM identity)"
            ),
            "{r}"
        );
        let r = assess(&sql_on(Some("sales"), "delete from t"), Some(&f), false)
            .reason
            .unwrap();
        assert!(
            r.contains("Redshift · analytics-wg · sales · eu-west-1")
                && r.contains("not this connection's default database 'dev'")
                && r.starts_with("PRODUCTION connection 'warehouse'"),
            "{r}"
        );
        let t = approval_target(&sql_on(Some(" sales "), "select 1"), &f).unwrap();
        assert_eq!(t.engine, EngineKind::Redshift);
        assert_eq!(
            (t.database.as_deref(), t.default_database.as_deref()),
            (Some("sales"), Some("dev"))
        );
        let t = approval_target(&sql_on(None, "select 1"), &f).unwrap();
        assert_eq!(t.database.as_deref(), Some("dev"));
    }

    #[test]
    fn facts_carry_the_redshift_database_and_target() {
        use crate::model::{AwsIdentity, AwsTarget, RedshiftAuth, RedshiftTarget};
        let opts = RedshiftOptions {
            target: RedshiftTarget::Provisioned {
                cluster_identifier: "dw".into(),
            },
            database: "analytics".into(),
            aws: AwsTarget {
                identity: AwsIdentity::DefaultChain,
                region: "us-east-1".into(),
            },
            auth: RedshiftAuth::IamIdentity,
        };
        let p = ConnectionProfile {
            id: "r".into(),
            name: "dw".into(),
            engine: EngineKind::Redshift,
            host: String::new(),
            port: 0,
            // A stale Postgres field is not the Redshift database.
            database: Some("ignored".into()),
            auth: crate::model::AuthMethod::None,
            tls: Default::default(),
            tunnel: None,
            environment: Environment::Dev,
            ai_data_access: Results,
            color: None,
            pg: Default::default(),
            valkey: Default::default(),
            created_at: None,
            updated_at: None,
            iceberg: None,
            redshift: Some(opts.clone()),
            mysql: Default::default(),
        };
        let f = ProfileFacts::from(&p);
        assert_eq!(f.database.as_deref(), Some("analytics"));
        assert_eq!(f.redshift, Some(opts));
        let mut pg = p.clone();
        pg.engine = EngineKind::Postgres;
        assert_eq!(ProfileFacts::from(&pg).redshift, None);
    }

    fn my(access: AiDataAccess, env: Environment) -> ProfileFacts {
        ProfileFacts {
            name: "orders-mysql".into(),
            engine: EngineKind::Mysql,
            environment: env,
            access,
            athena: false,
            remote_engine: None,
            remote: None,
            mysql_detail: Some("MySQL · db.internal:3306 · as app".into()),
            database: Some("shop".into()),
            redshift: None,
        }
    }

    #[test]
    fn mysql_reads_follow_the_access_matrix() {
        let read = "SELECT id FROM orders LIMIT 5";
        assert_eq!(
            assess(&sql(read), Some(&my(Results, Environment::Dev)), false).risk,
            Safe
        );
        assert_eq!(
            assess(
                &sql(read),
                Some(&my(ResultsWithApproval, Environment::Dev)),
                false
            )
            .risk,
            NeedsApproval
        );
        assert_eq!(
            assess(&sql(read), Some(&my(SchemaOnly, Environment::Dev)), false).risk,
            Denied
        );
        let r = assess(
            &sql(read),
            Some(&my(AiDataAccess::None, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, Denied);
        // A write needs approval and runs writable; on schema-only its rows are withheld.
        let w = assess(
            &sql("UPDATE orders SET status = 'paid' WHERE id = 1"),
            Some(&my(SchemaOnly, Environment::Prod)),
            false,
        );
        assert_eq!(w.risk, NeedsApproval);
        assert!(w.allow_write && w.redact_values);
        assert!(
            w.reason
                .unwrap()
                .starts_with("PRODUCTION connection 'orders-mysql'")
        );
    }

    /// What a READ ONLY transaction does not stop is not run as a read: a function outside the
    /// built-in read-only list, a locking read or an executable comment goes to approval or is
    /// refused, at every access level.
    #[test]
    fn mysql_reads_with_side_effects_are_not_safe() {
        for s in [
            "SELECT SLEEP(10)",
            "SELECT GET_LOCK('x', 10)",
            "SELECT shop.do_things()",
            "SELECT * FROM orders FOR UPDATE",
            "SELECT @a := 1",
            "SELECT /*+ SET_VAR(sort_buffer_size = 1) */ 1",
        ] {
            let r = assess(&sql(s), Some(&my(Results, Environment::Dev)), false);
            assert_eq!(r.risk, NeedsApproval, "{s}");
            assert!(r.allow_write, "{s}");
        }
        for (s, needle) in [
            ("SELECT * FROM orders INTO OUTFILE '/tmp/o'", "OUTFILE"),
            ("SELECT LOAD_FILE('/etc/passwd')", "LOAD_FILE"),
            (
                "LOAD DATA INFILE '/var/lib/mysql-files/x' INTO TABLE t",
                "LOAD DATA",
            ),
            ("SET GLOBAL general_log = 1", "SET GLOBAL"),
            ("KILL 42", "KILL"),
            ("USE other", "USE"),
            ("SELECT 1 /*!50000 , SLEEP(5) */", "executable comment"),
            ("DELIMITER //", "DELIMITER"),
        ] {
            for a in [SchemaOnly, ResultsWithApproval, Results] {
                let r = assess(&sql(s), Some(&my(a, Environment::Dev)), false);
                assert_eq!(r.risk, Denied, "{s}");
                assert!(
                    r.reason.as_deref().unwrap_or("").contains(needle),
                    "{s}: {:?}",
                    r.reason
                );
            }
        }
        let r = assess(
            &sql("SET @x = 1"),
            Some(&my(Results, Environment::Dev)),
            false,
        );
        assert_eq!(r.risk, Denied);
        assert!(r.reason.unwrap().contains("SET SESSION"));
    }

    #[test]
    fn mysql_approval_names_host_user_and_database() {
        let f = my(ResultsWithApproval, Environment::Dev);
        let t = approval_target(&sql_on(Some("archive"), "SELECT 1"), &f).unwrap();
        assert_eq!(t.engine, EngineKind::Mysql);
        assert_eq!(
            (t.database.as_deref(), t.default_database.as_deref()),
            (Some("archive"), Some("shop"))
        );
        assert_eq!(
            t.detail.as_deref(),
            Some("MySQL · db.internal:3306 · as app")
        );
        let r = assess(&sql_on(Some("archive"), "delete from t"), Some(&f), false)
            .reason
            .unwrap();
        assert!(
            r.contains(
                "'orders-mysql', database 'archive' (not this connection's default database 'shop')"
            ),
            "{r}"
        );
        let r = assess(
            &ToolCall::ListDatabases {
                profile_id: "p".into(),
            },
            Some(&f),
            false,
        );
        assert_eq!(r.risk, Safe);
    }

    #[test]
    fn facts_carry_the_mysql_database_and_detail() {
        let p: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "orders-mysql", "engine": "mysql", "host": "db.internal",
            "port": 3306, "database": "shop", "auth": {"kind": "password", "username": "app"},
            "tunnel": null, "color": null, "createdAt": null, "updatedAt": null,
        }))
        .unwrap();
        let f = ProfileFacts::from(&p);
        assert_eq!(f.database.as_deref(), Some("shop"));
        assert_eq!(
            f.mysql_detail.as_deref(),
            Some("MySQL · db.internal:3306 · as app")
        );
        let mut pg = p.clone();
        pg.engine = EngineKind::Postgres;
        assert_eq!(ProfileFacts::from(&pg).mysql_detail, None);
    }
}
