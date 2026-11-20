//! Valkey / Redis OSS engine (OWNER: valkey agent).
//!
//! - `redis` crate 1.x, RESP3, one multiplexed connection **per logical database** (standalone)
//!   or one cluster connection. See `session.rs` for the lifecycle and retry policy.
//! - Credentials come from the [`CredentialFn`] on **every** (re)build, so an ElastiCache IAM
//!   token is always fresh. An auth error or a dropped connection rebuilds the connection once
//!   and retries (reads always; writes only after an auth rejection, because a write may have
//!   been applied before a socket died).
//! - Write detection is **server-authoritative**: `COMMAND INFO <name>` flags (`write`,
//!   `admin`, `pubsub`, `blocking`, `@scripting`), cached per connection, plus a small static
//!   read-only table (`cmdinfo.rs`) that is needed because the server gives `CONFIG GET` and
//!   `CONFIG SET` identical flags. Unknown commands count as writes.
//! - SCAN never KEYS. Cluster mode scans each primary in turn; the cursor is the opaque string
//!   `"<nodeIndex>:<nodeCursor>"`. Cluster mode only has db 0.
//! - Logical DBs 0..N come from `CONFIG GET databases` (often disabled on ElastiCache → fall back
//!   to 16, extended to cover any db that `INFO keyspace` shows).
//!
//! ## TLS
//! Standalone: we open the socket and run rustls ourselves with [`crate::tls::client_config`], so
//! dialling `params.host:params.port` while verifying `params.tls_server_name` works, as do the
//! RDS bundle, a custom CA and `Require` (encrypt, no verification).
//!
//! Cluster: the `redis` crate owns the TLS handshake and cannot be handed a `ClientConfig` or a
//! stream. Consequences, none of which silently weaken verification:
//! - the seed node is addressed by `tls_server_name` and a pinned DNS resolver dials the real
//!   host, so dial/verify separation works for the seed connection;
//! - trust roots are the OS roots plus `ca_cert_path`; the bundled RDS CA set is **not** applied
//!   (ElastiCache certificates chain to public Amazon roots, which OS stores carry);
//! - `Require` is rejected with a clear error unless `redis` is built with `tls-rustls-insecure`.
//!   Cluster nodes discovered via `CLUSTER SLOTS` are dialled at the addresses the cluster
//!   advertises and verified against those names.
//!
//! ## Known limitations
//! - Keys are addressed as `&str`; a non-UTF-8 key is shown lossily and cannot be fetched back.
//! - `MULTI`/`WATCH`, pub/sub, `MONITOR`, `HELLO`, `AUTH`, `SELECT` are refused in `command()`.
//! - So are blocking commands (`BLPOP`, `BZPOPMIN`, `XREAD BLOCK`, `WAIT` and the rest of
//!   [`is_blocking_command`], the list the agent's policy uses too). One would hold up every
//!   other caller of the same multiplexed connection, and after the reply timeout it would still
//!   be waiting on the server, where it can take the next element for nobody. A command that
//!   only carries the server's `blocking` flag and is not on that list (a module's, say) is
//!   still run, after confirmation.
//! - A cluster scan stops with "topology changed" when the primaries are no longer the ones it
//!   started with (see [`ScanCursor`]). Slots that move between the same primaries during a
//!   scan are not noticed; keys in them can be missed or seen twice, as with `SCAN` itself.

mod cmdinfo;
mod convert;
mod read;
mod session;
mod transport;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use redis::cluster_routing::SingleNodeRoutingInfo;
use redis::{Cmd, Value, cmd, pipe};

pub(crate) use self::cmdinfo::is_blocking_command;
use self::cmdinfo::{Classification, CommandMeta};
use self::convert::*;
use self::session::Session;
use crate::error::{CoreError, Result};
use crate::model::*;
use crate::ports::CredentialFn;

/// Where and how to connect. `host:port` is dialled; `tls_server_name` is what the server
/// certificate must match (they differ when going through an SSH tunnel to `127.0.0.1`).
#[derive(Debug, Clone)]
pub struct ValkeyConnectParams {
    pub host: String,
    pub port: u16,
    pub tls_server_name: String,
    pub tls: TlsConfig,
    pub options: ValkeyOptions,
}

/// Default number of logical databases when `CONFIG GET databases` is not available.
const DEFAULT_DB_COUNT: usize = 16;

/// How long a refusal of `COMMAND INFO` is believed before the server is asked again. Until
/// then the static table decides, which errs on the side of "this writes".
const COMMAND_INFO_RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Default)]
struct MetaCache {
    /// `name` or `name|sub` (lowercase) → what `COMMAND INFO` said (`None` = unknown command).
    commands: HashMap<String, Option<CommandMeta>>,
    /// `COMMAND INFO` itself was refused or is missing; use the static table.
    unavailable: Option<Refused>,
}

/// When `COMMAND INFO` was refused, and how many connections the session had built by then.
#[derive(Clone, Copy)]
struct Refused {
    at: Instant,
    connections: u64,
}

impl MetaCache {
    /// Whether to go on believing that `COMMAND INFO` is refused. Not for the life of the
    /// handle: the server also refuses for reasons that pass (it is loading its data, a script
    /// is running, the user's ACL has since been changed). The refusal is dropped once the
    /// session has built a new connection, or after [`COMMAND_INFO_RETRY_AFTER`], and the next
    /// lookup asks the server again.
    fn command_info_refused(&mut self, now: Instant, connections: u64) -> bool {
        let still = self.unavailable.is_some_and(|r| {
            r.connections == connections
                && now.saturating_duration_since(r.at) < COMMAND_INFO_RETRY_AFTER
        });
        if !still {
            self.unavailable = None;
        }
        still
    }

    fn refuse_command_info(&mut self, now: Instant, connections: u64) {
        self.unavailable = Some(Refused {
            at: now,
            connections,
        });
    }
}

enum Lookup {
    Found(Option<CommandMeta>),
    Unavailable,
}

/// A connected Valkey/Redis server (standalone or cluster).
pub struct ValkeyHandle {
    session: Session,
    meta: std::sync::Mutex<MetaCache>,
}

impl ValkeyHandle {
    /// Connects, authenticates (credentials from `creds`, `None` username/password → no auth)
    /// and verifies the server speaks RESP3, so configuration errors surface here and not on
    /// first use.
    pub async fn connect(
        params: ValkeyConnectParams,
        creds: CredentialFn,
    ) -> Result<Arc<ValkeyHandle>> {
        // Only the standalone path uses our own rustls config; building it also validates the
        // CA settings early.
        let tls = if params.options.cluster {
            None
        } else {
            crate::tls::client_config(&params.tls)?
        };
        let session = Session::new(params, creds, tls);
        session.warm_up(0).await?;
        Ok(Arc::new(ValkeyHandle {
            session,
            meta: std::sync::Mutex::new(MetaCache::default()),
        }))
    }

    fn cluster(&self) -> bool {
        self.session.is_cluster()
    }

    /// In cluster mode, commands without a key go to any one node.
    fn any_node(&self) -> Option<SingleNodeRoutingInfo> {
        self.cluster().then_some(SingleNodeRoutingInfo::Random)
    }

    /// `INFO server/memory/replication/keyspace` → [`ValkeyServerInfo`].
    pub async fn server_info(&self) -> Result<ValkeyServerInfo> {
        let route = self.any_node();
        let multi = self
            .session
            .exec(
                0,
                cmd("INFO")
                    .arg("server")
                    .arg("memory")
                    .arg("replication")
                    .arg("keyspace"),
                route.as_ref(),
                true,
            )
            .await;
        let reply = match multi {
            Ok(v) => v,
            // Servers older than 7.0 accept a single section only; plain INFO has all we need.
            Err(CoreError::Valkey(_)) => {
                self.session
                    .exec(0, &cmd("INFO"), route.as_ref(), true)
                    .await?
            }
            Err(e) => return Err(e),
        };
        let text = value_text(&reply)
            .ok_or_else(|| CoreError::Valkey("unexpected reply to INFO".into()))?;
        Ok(server_info_from(parse_info(&text)))
    }

    /// Logical databases with key counts. Cluster mode: just db 0.
    pub async fn list_dbs(&self) -> Result<Vec<ValkeyDbInfo>> {
        if self.cluster() {
            // DBSIZE is aggregated (summed) across primaries by the cluster client.
            let keys = match self.session.exec(0, &cmd("DBSIZE"), None, true).await {
                Ok(v) => value_i64(&v).and_then(|n| u64::try_from(n).ok()),
                Err(_) => None,
            };
            return Ok(vec![ValkeyDbInfo { index: 0, keys }]);
        }

        let configured = match self
            .session
            .exec(0, cmd("CONFIG").arg("GET").arg("databases"), None, true)
            .await
        {
            Ok(v) => text_pairs(&v)
                .into_iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("databases"))
                .and_then(|(_, n)| n.trim().parse::<usize>().ok()),
            // CONFIG is typically disabled on ElastiCache; that is not an error.
            Err(CoreError::Valkey(_)) => None,
            Err(e) => return Err(e),
        };
        let keyspace = match self
            .session
            .exec(0, cmd("INFO").arg("keyspace"), None, true)
            .await
        {
            Ok(v) => value_text(&v).map(|t| parse_keyspace(&parse_info(&t))),
            Err(CoreError::Valkey(_)) => None,
            Err(e) => return Err(e),
        };
        Ok(build_db_list(configured, keyspace))
    }

    /// One SCAN page (with pipelined TYPE per key). Pass `"0"` to start.
    pub async fn scan(&self, db: u8, pattern: &str, cursor: &str, count: u32) -> Result<ScanPage> {
        let cluster = self.cluster();
        let db = self.session.check_db(db)?;
        let position = ScanCursor::decode(cursor, cluster)?;
        let count = count.clamp(1, 100_000);

        // Cluster: the primary this page reads, and the primaries as they are now.
        let (route, index, primaries) = if cluster {
            let nodes = self.primaries().await?;
            let index = locate(&position, &nodes)?;
            let (host, port) = nodes[index].clone();
            (
                Some(SingleNodeRoutingInfo::ByAddress { host, port }),
                index,
                nodes,
            )
        } else {
            (None, 0, Vec::new())
        };

        let mut c = cmd("SCAN");
        c.arg(position.cursor);
        if !pattern.is_empty() && pattern != "*" {
            c.arg("MATCH").arg(pattern);
        }
        c.arg("COUNT").arg(count);
        let reply = self.session.exec(db, &c, route.as_ref(), true).await?;

        let (next, raw_keys) = parse_scan_reply(&reply)?;

        let mut types: Vec<Option<KeyType>> = vec![None; raw_keys.len()];
        if !raw_keys.is_empty() {
            let mut p = pipe();
            for k in &raw_keys {
                p.cmd("TYPE").arg(k.as_slice());
            }
            let replies = self.session.pipeline(db, &p, route.as_ref()).await?;
            for (slot, r) in types.iter_mut().zip(&replies) {
                // A key deleted between SCAN and TYPE answers "none"; show it as unknown.
                *slot = value_text(r).map(|t| parse_key_type(&t));
            }
        }
        let keys = raw_keys
            .iter()
            .zip(types)
            .map(|(k, key_type)| KeyEntry {
                key: lossy(k),
                key_type,
            })
            .collect();

        let (next_position, done) = advance(index, next, &primaries);
        Ok(ScanPage {
            keys,
            cursor: next_position.encode(cluster),
            done,
        })
    }

    /// Primary nodes in a stable order (sorted by host, port), from `CLUSTER SLOTS`.
    async fn primaries(&self) -> Result<Vec<(String, u16)>> {
        let route = SingleNodeRoutingInfo::Random;
        let reply = self
            .session
            .exec(0, cmd("CLUSTER").arg("SLOTS"), Some(&route), true)
            .await?;
        let nodes = parse_primaries(&reply, &self.session.params.host)?;
        if nodes.is_empty() {
            return Err(CoreError::Valkey(
                "CLUSTER SLOTS returned no primaries".into(),
            ));
        }
        Ok(nodes)
    }

    /// Reads a key's value with TTL, capping collections at `max_items` elements.
    pub async fn get_value(&self, db: u8, key: &str, max_items: usize) -> Result<ValkeyValue> {
        let db = self.session.check_db(db)?;
        read::get_value(&self.session, db, key, max_items).await
    }

    /// Whether `argv` would modify data or server state, as judged by the server's own
    /// `COMMAND INFO` (cached per command). Unknown → `true`.
    pub async fn is_write_command(&self, argv: &[String]) -> Result<bool> {
        Ok(self.classify(argv).await?.write)
    }

    async fn lookup_meta(&self, key: &str) -> Result<Lookup> {
        {
            let mut cache = self.meta.lock().unwrap_or_else(|e| e.into_inner());
            if cache.command_info_refused(Instant::now(), self.session.connections_made()) {
                return Ok(Lookup::Unavailable);
            }
            if let Some(found) = cache.commands.get(key) {
                return Ok(Lookup::Found(found.clone()));
            }
        }
        let route = self.any_node();
        let reply = self
            .session
            .exec(0, cmd("COMMAND").arg("INFO").arg(key), route.as_ref(), true)
            .await;
        let mut cache = self.meta.lock().unwrap_or_else(|e| e.into_inner());
        match reply {
            Ok(v) => {
                let meta = cmdinfo::parse_command_info(&v);
                cache.commands.insert(key.to_owned(), meta.clone());
                Ok(Lookup::Found(meta))
            }
            // The server answered with an error (unknown command, NOPERM): COMMAND INFO is not
            // usable here, for now (see `MetaCache::command_info_refused`). Connection-level
            // failures are not cached; they propagate.
            Err(CoreError::Valkey(_)) => {
                cache.refuse_command_info(Instant::now(), self.session.connections_made());
                Ok(Lookup::Unavailable)
            }
            Err(e) => Err(e),
        }
    }

    async fn classify(&self, argv: &[String]) -> Result<Classification> {
        let first = argv.first().map(|s| s.trim()).filter(|s| !s.is_empty());
        let Some(first) = first else {
            return Err(CoreError::Invalid("empty command".into()));
        };
        let name = first.to_ascii_lowercase();
        let sub = argv.get(1).map(|s| s.to_ascii_lowercase());
        let upper = |s: &str| s.to_ascii_uppercase();
        let display_plain = upper(&name);
        let display_sub = sub
            .as_deref()
            .map(|s| format!("{} {}", upper(&name), upper(s)));

        let parent = match self.lookup_meta(&name).await? {
            Lookup::Found(Some(m)) => m,
            Lookup::Found(None) => return Ok(cmdinfo::classify_unknown(&display_plain)),
            Lookup::Unavailable => {
                let d = display_sub
                    .as_deref()
                    .filter(|_| sub.is_some())
                    .unwrap_or(&display_plain);
                return Ok(cmdinfo::classify_fallback(d, &name, sub.as_deref()));
            }
        };

        if parent.has_subcommands
            && let (Some(s), Some(d)) = (sub.as_deref(), display_sub.as_deref())
        {
            return match self.lookup_meta(&format!("{name}|{s}")).await? {
                Lookup::Found(Some(m)) => Ok(cmdinfo::classify_with_meta(d, &name, Some(s), &m)),
                Lookup::Found(None) if cmdinfo::is_read_listed(&name, Some(s)) => {
                    Ok(cmdinfo::classify_with_meta(d, &name, Some(s), &parent))
                }
                Lookup::Found(None) => Ok(cmdinfo::classify_unknown(d)),
                Lookup::Unavailable => Ok(cmdinfo::classify_fallback(d, &name, Some(s))),
            };
        }
        // Redis 6 has no subcommand info: the parent's flags plus the static table decide.
        let s = sub
            .as_deref()
            .filter(|_| cmdinfo::is_read_listed(&name, sub.as_deref()));
        let d = if s.is_some() {
            display_sub.as_deref().unwrap_or(&display_plain)
        } else {
            &display_plain
        };
        Ok(cmdinfo::classify_with_meta(d, &name, s, &parent))
    }

    /// Raw command for the console. Returns `RequiresConfirmation` for writes unless `allow_write`.
    pub async fn command(
        &self,
        db: u8,
        argv: Vec<String>,
        allow_write: bool,
    ) -> Result<CommandOutcome> {
        if argv.iter().all(|a| a.trim().is_empty()) {
            return Err(CoreError::Invalid("empty command".into()));
        }
        if let Some(msg) = cmdinfo::blocked_reason(&argv) {
            return Err(CoreError::Invalid(msg));
        }
        let db = self.session.check_db(db)?;
        let class = self.classify(&argv).await?;
        if class.write && !allow_write {
            return Ok(CommandOutcome::RequiresConfirmation {
                reason: class.reason(),
            });
        }

        let mut c = Cmd::new();
        for a in &argv {
            c.arg(a.as_str());
        }
        let started = Instant::now();
        // Only a command proven read-only may be replayed after a dropped connection.
        let reply = self.session.exec(db, &c, None, !class.write).await?;
        Ok(CommandOutcome::Ok {
            result: value_to_json(&reply),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Seconds since the connection was last used (for the idle reaper).
    pub fn idle_secs(&self) -> u64 {
        self.session.idle_secs()
    }

    /// Drops every connection. Later calls fail with `NotConnected`.
    pub async fn close(&self) {
        self.session.close().await;
    }
}

// ----------------------------------------------------------------------------------------
// Pure helpers (unit-tested below)
// ----------------------------------------------------------------------------------------

fn server_info_from(mut info: std::collections::BTreeMap<String, String>) -> ValkeyServerInfo {
    let version = info
        .remove("valkey_version")
        .or_else(|| info.get("redis_version").cloned());
    let mode = info
        .remove("server_mode")
        .or_else(|| info.get("redis_mode").cloned());
    let role = info.remove("role");
    let used_memory_human = info.remove("used_memory_human");
    ValkeyServerInfo {
        version,
        mode,
        role,
        used_memory_human,
        extra: info,
    }
}

/// `databases` is the configured count (if known); `keyspace` maps db index → key count (if the
/// INFO call worked). The list always covers every db the keyspace mentions.
fn build_db_list(
    configured: Option<usize>,
    keyspace: Option<std::collections::BTreeMap<u8, u64>>,
) -> Vec<ValkeyDbInfo> {
    let highest = keyspace
        .as_ref()
        .and_then(|k| k.keys().next_back().map(|i| usize::from(*i) + 1))
        .unwrap_or(0);
    let count = configured
        .unwrap_or(DEFAULT_DB_COUNT)
        .max(highest)
        .clamp(1, 256);
    (0..count)
        .map(|i| {
            let index = i as u8; // count <= 256, so i <= 255
            let keys = keyspace
                .as_ref()
                .map(|k| k.get(&index).copied().unwrap_or(0));
            ValkeyDbInfo { index, keys }
        })
        .collect()
}

/// `SCAN` reply → (next cursor, raw key bytes).
fn parse_scan_reply(reply: &Value) -> Result<(u64, Vec<Vec<u8>>)> {
    let bad = || CoreError::Valkey(format!("unexpected reply to SCAN: {reply:?}"));
    let parts = value_items(reply).ok_or_else(bad)?;
    let [cursor, keys] = parts else {
        return Err(bad());
    };
    let cursor = value_text(cursor)
        .and_then(|t| t.trim().parse::<u64>().ok())
        .ok_or_else(bad)?;
    let keys = value_items(keys).ok_or_else(bad)?;
    Ok((cursor, keys.iter().filter_map(value_bytes).collect()))
}

fn topology_changed() -> CoreError {
    CoreError::Invalid(
        "the cluster topology changed since this scan started; restart the scan".into(),
    )
}

/// Which of `primaries` (the cluster's primaries as they are now) a scan page reads.
///
/// The start is the first primary. After that the position names its node and the topology it
/// was handed out under: if the primaries are no longer that set, or the node is not among
/// them, the scan cannot be continued truthfully and fails.
fn locate(position: &ScanCursor, primaries: &[(String, u16)]) -> Result<usize> {
    let Some(node) = &position.node else {
        return Ok(0);
    };
    if node.topology != topology_fingerprint(primaries) {
        return Err(topology_changed());
    }
    primaries
        .iter()
        .position(|(host, port)| *host == node.host && *port == node.port)
        .ok_or_else(topology_changed)
}

/// Where the scan goes after a page from `primaries[index]` (no primaries: standalone). A node
/// cursor of 0 means that node is exhausted: move to the next primary, or finish after the last.
fn advance(index: usize, node_cursor: u64, primaries: &[(String, u16)]) -> (ScanCursor, bool) {
    let on = |index: usize, cursor: u64| ScanCursor {
        node: primaries.get(index).map(|(host, port)| ScanNode {
            host: host.clone(),
            port: *port,
            topology: topology_fingerprint(primaries),
        }),
        cursor,
    };
    if node_cursor != 0 {
        return (on(index, node_cursor), false);
    }
    if index + 1 < primaries.len() {
        (on(index + 1, 0), false)
    } else {
        (ScanCursor::START, true)
    }
}

/// Primary `(host, port)` pairs from a `CLUSTER SLOTS` reply, deduplicated and sorted.
/// `fallback_host` stands in for nodes that advertise an empty address.
fn parse_primaries(reply: &Value, fallback_host: &str) -> Result<Vec<(String, u16)>> {
    let ranges = value_items(reply).ok_or_else(|| {
        CoreError::Valkey(format!("unexpected reply to CLUSTER SLOTS: {reply:?}"))
    })?;
    let mut nodes = Vec::new();
    for range in ranges {
        // [start, end, [host, port, id, ...], replica...]: the first node entry is the primary.
        let Some(primary) = value_items(range)
            .and_then(|r| r.get(2))
            .and_then(value_items)
        else {
            continue;
        };
        let host = primary
            .first()
            .and_then(value_text)
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| fallback_host.to_owned());
        let Some(port) = primary
            .get(1)
            .and_then(value_i64)
            .and_then(|p| u16::try_from(p).ok())
        else {
            continue;
        };
        nodes.push((host, port));
    }
    nodes.sort();
    nodes.dedup();
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A refusal of `COMMAND INFO` used to be believed for the life of the connection handle.
    /// The server also refuses for reasons that pass (it is still loading its data, a script is
    /// running, the ACL has since been changed), and from then on every command that is not on
    /// the built-in read-only list counted as a write.
    #[test]
    fn a_refusal_of_command_info_is_believed_for_a_while_and_then_checked_again() {
        let mut cache = MetaCache::default();
        let t0 = Instant::now();
        assert!(!cache.command_info_refused(t0, 2), "nothing refused yet");

        cache.refuse_command_info(t0, 2);
        assert!(cache.command_info_refused(t0, 2));
        let almost = t0 + COMMAND_INFO_RETRY_AFTER - Duration::from_millis(1);
        assert!(
            cache.command_info_refused(almost, 2),
            "within the delay the static table decides, without asking the server every time"
        );
        assert!(
            !cache.command_info_refused(t0 + COMMAND_INFO_RETRY_AFTER, 2),
            "after the delay the server is asked again"
        );
        // Asking again and being refused again starts a new delay.
        let t1 = t0 + COMMAND_INFO_RETRY_AFTER;
        cache.refuse_command_info(t1, 2);
        assert!(cache.command_info_refused(t1 + Duration::from_secs(1), 2));
    }

    #[test]
    fn a_refusal_of_command_info_does_not_outlive_the_connection_it_came_from() {
        let mut cache = MetaCache::default();
        let t0 = Instant::now();
        cache.refuse_command_info(t0, 2);
        assert!(cache.command_info_refused(t0, 2));
        assert!(
            !cache.command_info_refused(t0, 3),
            "the session has built a new connection since: ask again"
        );
        assert!(
            !cache.command_info_refused(t0, 3),
            "and the old refusal is forgotten, not merely skipped once"
        );
    }

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn server_info_prefers_valkey_fields() {
        let info = parse_info(
            "valkey_version:8.0.1\r\nredis_version:7.2.4\r\nserver_mode:cluster\r\nredis_mode:standalone\r\nrole:master\r\nused_memory_human:1.50M\r\ntcp_port:6379\r\n",
        );
        let s = server_info_from(info);
        assert_eq!(s.version.as_deref(), Some("8.0.1"));
        assert_eq!(s.mode.as_deref(), Some("cluster"));
        assert_eq!(s.role.as_deref(), Some("master"));
        assert_eq!(s.used_memory_human.as_deref(), Some("1.50M"));
        assert_eq!(s.extra.get("tcp_port").map(String::as_str), Some("6379"));
        // the Redis compatibility version is kept for reference
        assert_eq!(
            s.extra.get("redis_version").map(String::as_str),
            Some("7.2.4")
        );
    }

    #[test]
    fn server_info_redis_fallback() {
        let s = server_info_from(parse_info(
            "redis_version:7.0.15\r\nredis_mode:standalone\r\n",
        ));
        assert_eq!(s.version.as_deref(), Some("7.0.15"));
        assert_eq!(s.mode.as_deref(), Some("standalone"));
        assert!(s.role.is_none());
    }

    #[test]
    fn db_list_default_and_keyspace() {
        let none = build_db_list(None, None);
        assert_eq!(none.len(), 16);
        assert!(none.iter().all(|d| d.keys.is_none()));

        let ks = BTreeMap::from([(0u8, 3u64), (14, 1200)]);
        let l = build_db_list(Some(16), Some(ks));
        assert_eq!(l.len(), 16);
        assert_eq!(
            l[0],
            ValkeyDbInfo {
                index: 0,
                keys: Some(3)
            }
        );
        assert_eq!(l[14].keys, Some(1200));
        assert_eq!(l[1].keys, Some(0));
    }

    #[test]
    fn db_list_grows_to_cover_keyspace_and_clamps() {
        let l = build_db_list(Some(4), Some(BTreeMap::from([(9u8, 1u64)])));
        assert_eq!(l.len(), 10);
        assert_eq!(build_db_list(Some(100_000), None).len(), 256);
        assert_eq!(build_db_list(Some(0), None).len(), 1);
        assert_eq!(
            build_db_list(Some(256), None).last().map(|d| d.index),
            Some(255)
        );
    }

    #[test]
    fn scan_reply_parsing() {
        let reply = Value::Array(vec![
            bulk("17"),
            Value::Array(vec![bulk("a"), Value::BulkString(vec![0xff, 0x01])]),
        ]);
        let (cur, keys) = parse_scan_reply(&reply).unwrap();
        assert_eq!(cur, 17);
        assert_eq!(keys, vec![b"a".to_vec(), vec![0xff, 0x01]]);
        assert!(parse_scan_reply(&Value::Nil).is_err());
        assert!(parse_scan_reply(&Value::Array(vec![bulk("x"), Value::Array(vec![])])).is_err());
    }

    #[test]
    fn a_standalone_scan_follows_the_server_cursor_until_it_is_zero() {
        let (c, done) = advance(0, 42, &[]);
        assert!(!done);
        assert_eq!(c.encode(false), "42");
        assert_eq!(ScanCursor::decode("42", false).unwrap(), c);
        assert_eq!(advance(0, 0, &[]), (ScanCursor::START, true));
    }

    // ---- cluster scans across topology changes (review finding M10) ----------------------

    fn primaries(addrs: &[&str]) -> Vec<(String, u16)> {
        let mut nodes: Vec<(String, u16)> = addrs
            .iter()
            .map(|a| {
                let (host, port) = a.rsplit_once(':').unwrap();
                (host.to_string(), port.parse().unwrap())
            })
            .collect();
        // `parse_primaries` hands them over sorted.
        nodes.sort();
        nodes
    }

    /// The position a scan is at after a page from `nodes[index]` that ended with
    /// `node_cursor`, as the next call gets it: encoded for the UI and decoded again.
    fn resumed(nodes: &[(String, u16)], index: usize, node_cursor: u64) -> ScanCursor {
        let (next, done) = advance(index, node_cursor, nodes);
        assert!(!done);
        ScanCursor::decode(&next.encode(true), true).unwrap()
    }

    fn is_topology_error(r: Result<usize>) -> bool {
        matches!(r, Err(CoreError::Invalid(m)) if m.contains("topology changed") && m.contains("restart the scan"))
    }

    #[test]
    fn a_cluster_scan_walks_every_primary_in_turn_then_finishes() {
        let nodes = primaries(&["10.0.0.1:6379", "10.0.0.2:6379", "10.0.0.3:6379"]);
        // The first call has no position yet: it starts at the first primary.
        assert_eq!(locate(&ScanCursor::START, &nodes).unwrap(), 0);
        // More to come from the first node: stay on it.
        assert_eq!(locate(&resumed(&nodes, 0, 9), &nodes).unwrap(), 0);
        // Its cursor ran to 0: on to the second, and from an empty second node to the third.
        assert_eq!(locate(&resumed(&nodes, 0, 0), &nodes).unwrap(), 1);
        assert_eq!(locate(&resumed(&nodes, 1, 0), &nodes).unwrap(), 2);
        assert_eq!(locate(&resumed(&nodes, 2, 7), &nodes).unwrap(), 2);
        // The last node is exhausted: done, and the cursor is the start again.
        assert_eq!(advance(2, 0, &nodes), (ScanCursor::START, true));
        // "0" is only ever the start: a position on a later node at node cursor 0 is not "0".
        assert_ne!(advance(0, 0, &nodes).0.encode(true), "0");
    }

    /// A failover: the second primary was replaced by its replica, which has another address.
    /// The cursor held an index, so the scan went on at the same index on a different node,
    /// with a node cursor that meant nothing there.
    #[test]
    fn a_cluster_scan_fails_when_the_node_it_was_on_is_no_longer_a_primary() {
        let before = primaries(&["10.0.0.1:6379", "10.0.0.2:6379", "10.0.0.3:6379"]);
        let position = resumed(&before, 1, 4711);
        let after = primaries(&["10.0.0.1:6379", "10.0.0.3:6379", "10.0.0.9:6379"]);
        assert!(
            is_topology_error(locate(&position, &after)),
            "{:?}",
            locate(&position, &after)
        );
        // Unchanged topology: the same node.
        assert_eq!(locate(&position, &before).unwrap(), 1);
    }

    /// The node the scan is on is still a primary, but the set of primaries is not the one the
    /// scan started with. The nodes already read and the nodes still to read are no longer
    /// well defined (slots have moved), so keys would be skipped or repeated without a word.
    #[test]
    fn a_cluster_scan_fails_when_the_set_of_primaries_changed() {
        let before = primaries(&["10.0.0.1:6379", "10.0.0.2:6379", "10.0.0.3:6379"]);
        let position = resumed(&before, 1, 4711);
        for after in [
            // a primary was added that sorts before the current one
            primaries(&[
                "10.0.0.0:6379",
                "10.0.0.1:6379",
                "10.0.0.2:6379",
                "10.0.0.3:6379",
            ]),
            // one was added after it
            primaries(&[
                "10.0.0.1:6379",
                "10.0.0.2:6379",
                "10.0.0.3:6379",
                "10.0.0.4:6379",
            ]),
            // an earlier one went away
            primaries(&["10.0.0.2:6379", "10.0.0.3:6379"]),
            // the same hosts on other ports
            primaries(&["10.0.0.1:6380", "10.0.0.2:6379", "10.0.0.3:6379"]),
        ] {
            assert!(
                is_topology_error(locate(&position, &after)),
                "{after:?}: {:?}",
                locate(&position, &after)
            );
        }
    }

    #[test]
    fn a_cluster_cursor_names_its_node_whatever_the_address_looks_like() {
        for addr in [
            "cache-0001.example.internal:6379",
            "::1:7101",
            "10.0.0.2:6379",
        ] {
            let nodes = primaries(&["a.example:1", addr]);
            let index = nodes
                .iter()
                .position(|n| *n == primaries(&[addr])[0])
                .unwrap();
            assert_eq!(
                locate(&resumed(&nodes, index, 4711), &nodes).unwrap(),
                index,
                "{addr}"
            );
        }
    }

    fn node(host: &str, port: i64) -> Value {
        Value::Array(vec![
            bulk(host),
            Value::Int(port),
            bulk("nodeid"),
            Value::Array(vec![]),
        ])
    }

    #[test]
    fn primaries_from_cluster_slots() {
        let reply = Value::Array(vec![
            Value::Array(vec![
                Value::Int(0),
                Value::Int(5460),
                node("10.0.0.2", 7001),
                node("10.0.0.5", 7004),
            ]),
            Value::Array(vec![
                Value::Int(5461),
                Value::Int(10922),
                node("10.0.0.1", 7000),
            ]),
            Value::Array(vec![
                Value::Int(10923),
                Value::Int(16383),
                node("10.0.0.2", 7001),
            ]),
            Value::Array(vec![Value::Int(1), Value::Int(2), node("", 7002)]),
        ]);
        let n = parse_primaries(&reply, "cache.local").unwrap();
        assert_eq!(
            n,
            vec![
                ("10.0.0.1".into(), 7000),
                ("10.0.0.2".into(), 7001),
                ("cache.local".into(), 7002)
            ]
        );
        assert!(parse_primaries(&Value::Nil, "h").is_err());
    }
}
