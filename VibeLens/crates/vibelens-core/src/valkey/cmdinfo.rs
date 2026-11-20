//! Write detection for the console and the agent policy.
//!
//! The server is the authority: `COMMAND INFO <name>` returns flags (`write`, `admin`,
//! `pubsub`, `blocking`, ...) and ACL categories (`@scripting`, ...). Two things make that
//! insufficient on its own, so this module adds a small, conservative layer:
//!
//! 1. Redis/Valkey give `CONFIG GET` and `CONFIG SET` (and `CLIENT LIST` / `CLIENT KILL`, ...)
//!    **identical** flags and categories. Distinguishing them needs the static [`READ_ONLY`]
//!    table. The table can only *downgrade* an `admin` / `pubsub` / `@scripting` verdict; a
//!    server-reported `write` flag always wins.
//! 2. `COMMAND INFO` may be unavailable (ACL-restricted users, some managed services). Then the
//!    table is used as an *allow-list*: listed → read, everything else → write. A refusal is
//!    not believed for ever: the server is asked again after a minute or once the connection
//!    has been rebuilt (`MetaCache::command_info_refused` in `mod.rs`).
//!
//! Anything unknown to the server is treated as a write (fail safe).

use redis::Value;

use super::convert::{value_items, value_text};

/// What `COMMAND INFO` told us about one command (or `command|subcommand`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandMeta {
    /// Lowercase flags, e.g. `write`, `admin`, `noscript`.
    pub flags: Vec<String>,
    /// Lowercase ACL categories including the `@`, e.g. `@scripting`.
    pub categories: Vec<String>,
    /// The command has subcommands (Redis 7+ / Valkey), so `argv[1]` selects one.
    pub has_subcommands: bool,
}

/// The verdict plus enough context to explain it to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Classification {
    pub write: bool,
    /// `CONFIG SET`, `SET`, ...
    pub display: String,
    pub flags: Vec<String>,
    /// Short human reason (`has the "write" flag`, `unknown to the server`, ...).
    pub why: String,
}

impl Classification {
    /// Message for `CommandOutcome::RequiresConfirmation`.
    pub fn reason(&self) -> String {
        let flags = if self.flags.is_empty() {
            String::new()
        } else {
            format!(" (server flags: {})", self.flags.join(", "))
        };
        format!(
            "{} may change data or server state: {}{flags}. Confirm to run it.",
            self.display, self.why
        )
    }
}

/// Parses the reply of `COMMAND INFO <name>`: `[ [name, arity, flags, first, last, step,
/// categories, tips, key_specs, subcommands], ... ]`. `None` when the server does not know the
/// command (it answers with a nil entry) or the shape is unexpected.
pub(super) fn parse_command_info(reply: &Value) -> Option<CommandMeta> {
    let entries = value_items(reply)?;
    let entry = value_items(entries.first()?)?;
    // [0] name, [1] arity, [2] flags are present on every server version that has COMMAND INFO.
    let flags = lower_strings(entry.get(2)?);
    let categories = entry.get(6).map(lower_strings).unwrap_or_default();
    let has_subcommands = entry
        .get(9)
        .and_then(value_items)
        .is_some_and(|s| !s.is_empty());
    Some(CommandMeta {
        flags,
        categories,
        has_subcommands,
    })
}

fn lower_strings(v: &Value) -> Vec<String> {
    value_items(v)
        .map(|items| {
            items
                .iter()
                .filter_map(value_text)
                .map(|s| s.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Verdict for a command the server described.
pub(super) fn classify_with_meta(
    display: &str,
    name: &str,
    sub: Option<&str>,
    meta: &CommandMeta,
) -> Classification {
    let has = |f: &str| meta.flags.iter().any(|x| x == f);
    let cat = |c: &str| meta.categories.iter().any(|x| x == c);
    let mk = |write: bool, why: &str| Classification {
        write,
        display: display.to_owned(),
        flags: meta.flags.clone(),
        why: why.to_owned(),
    };

    if has("write") {
        return mk(true, "it has the \"write\" flag");
    }
    if is_read_listed(name, sub) {
        return mk(false, "known read-only command");
    }
    if has("admin") {
        return mk(true, "it is an administrative command");
    }
    if has("pubsub") {
        return mk(true, "it publishes or manages pub/sub state");
    }
    if has("blocking") {
        return mk(true, "it can block the connection");
    }
    if cat("@scripting") {
        return mk(true, "it runs or manages scripts, which can write");
    }
    mk(false, "read-only")
}

/// Verdict for a command the server does not know. Fail safe.
pub(super) fn classify_unknown(display: &str) -> Classification {
    Classification {
        write: true,
        display: display.to_owned(),
        flags: Vec::new(),
        why: "the server does not recognise it, so it cannot be proven read-only".into(),
    }
}

/// Verdict when `COMMAND INFO` is unavailable: allow-list of known reads, everything else writes.
pub(super) fn classify_fallback(display: &str, name: &str, sub: Option<&str>) -> Classification {
    if is_read_listed(name, sub) {
        Classification {
            write: false,
            display: display.to_owned(),
            flags: Vec::new(),
            why: "known read-only command".into(),
        }
    } else {
        Classification {
            write: true,
            display: display.to_owned(),
            flags: Vec::new(),
            why: "the server's COMMAND INFO is unavailable and it is not on the built-in read-only list".into(),
        }
    }
}

/// Commands whose first argument selects a subcommand.
const PARENTS: &[&str] = &[
    "config", "client", "slowlog", "latency", "memory", "acl", "command", "module", "function",
    "script", "object", "xinfo", "pubsub", "cluster", "debug", "xgroup", "lolwut",
];

/// Commands (or `command|subcommand`s) that are known not to write. Used to downgrade
/// admin/pubsub/scripting verdicts and as the fallback allow-list. Deliberately excludes
/// blocking reads such as `XREAD` and `BLPOP`.
const READ_ONLY: &[&str] = &[
    // strings
    "get",
    "mget",
    "strlen",
    "getrange",
    "substr",
    "lcs",
    "getbit",
    "bitcount",
    "bitpos",
    // keys
    "exists",
    "type",
    "ttl",
    "pttl",
    "expiretime",
    "pexpiretime",
    "dump",
    "scan",
    "keys",
    "randomkey",
    "dbsize",
    "sort_ro",
    // hashes
    "hget",
    "hmget",
    "hgetall",
    "hkeys",
    "hvals",
    "hlen",
    "hexists",
    "hstrlen",
    "hscan",
    "hrandfield",
    // lists
    "lrange",
    "llen",
    "lindex",
    "lpos",
    // sets
    "smembers",
    "scard",
    "sismember",
    "smismember",
    "sscan",
    "srandmember",
    "sinter",
    "sunion",
    "sdiff",
    "sintercard",
    // sorted sets
    "zrange",
    "zrangebyscore",
    "zrangebylex",
    "zrevrange",
    "zrevrangebyscore",
    "zrevrangebylex",
    "zcard",
    "zcount",
    "zlexcount",
    "zscore",
    "zmscore",
    "zrank",
    "zrevrank",
    "zscan",
    "zrandmember",
    "zinter",
    "zunion",
    "zdiff",
    "zintercard",
    // streams
    "xrange",
    "xrevrange",
    "xlen",
    "xpending",
    "xinfo|stream",
    "xinfo|groups",
    "xinfo|consumers",
    "xinfo|help",
    // hyperloglog, geo
    "pfcount",
    "geopos",
    "geodist",
    "geohash",
    "geosearch",
    "georadius_ro",
    "georadiusbymember_ro",
    // server
    "ping",
    "echo",
    "time",
    "lastsave",
    "info",
    "role",
    "command",
    "command|info",
    "command|count",
    "command|docs",
    "command|list",
    "command|getkeys",
    "command|getkeysandflags",
    "command|help",
    "config|get",
    "config|help",
    "client|list",
    "client|info",
    "client|getname",
    "client|id",
    "client|trackinginfo",
    "client|getredir",
    "client|help",
    "slowlog|get",
    "slowlog|len",
    "slowlog|help",
    "latency|latest",
    "latency|history",
    "latency|graph",
    "latency|doctor",
    "latency|histogram",
    "latency|help",
    "memory|usage",
    "memory|stats",
    "memory|doctor",
    "memory|malloc-stats",
    "memory|help",
    "acl|whoami",
    "acl|list",
    "acl|users",
    "acl|cat",
    "acl|getuser",
    "acl|genpass",
    "acl|dryrun",
    "acl|help",
    "module|list",
    "module|help",
    "function|list",
    "function|stats",
    "function|dump",
    "function|help",
    "script|exists",
    "script|help",
    "object|encoding",
    "object|freq",
    "object|idletime",
    "object|refcount",
    "object|help",
    "pubsub|channels",
    "pubsub|numsub",
    "pubsub|numpat",
    "pubsub|shardchannels",
    "pubsub|shardnumsub",
    "pubsub|help",
    "cluster|info",
    "cluster|nodes",
    "cluster|slots",
    "cluster|shards",
    "cluster|keyslot",
    "cluster|countkeysinslot",
    "cluster|getkeysinslot",
    "cluster|myid",
    "cluster|myshardid",
    "cluster|links",
    "cluster|count-failure-reports",
    "cluster|replicas",
    "cluster|slaves",
    "cluster|help",
    // scripts the server itself guarantees are read-only
    "eval_ro",
    "evalsha_ro",
    "fcall_ro",
    // json module reads
    "json.get",
    "json.mget",
    "json.type",
    "json.strlen",
    "json.arrlen",
    "json.objlen",
    "json.objkeys",
    "json.resp",
    "json.arrindex",
];

/// `true` if `(name, sub)` is on the built-in read-only list. `name` and `sub` must be lowercase.
pub(super) fn is_read_listed(name: &str, sub: Option<&str>) -> bool {
    let is_parent = PARENTS.contains(&name);
    match (is_parent, sub) {
        (true, Some(s)) => {
            let key = format!("{name}|{s}");
            READ_ONLY.contains(&key.as_str())
        }
        // Bare parents: only COMMAND (list all) is meaningful and read-only.
        (true, None) => READ_ONLY.contains(&name),
        (false, _) => READ_ONLY.contains(&name),
    }
}

/// Commands that wait on the server until something happens or a timeout ends. This is the one
/// list of them: the console ([`blocked_reason`]) and the agent's policy both refuse what it
/// names. `XREAD` and `XREADGROUP` block only with `BLOCK`; any argument spelled `BLOCK` counts,
/// which errs on the side of refusing.
pub(crate) fn is_blocking_command(argv: &[String]) -> bool {
    let Some(name) = argv.first().map(|a| a.to_ascii_uppercase()) else {
        return false;
    };
    match name.as_str() {
        "BLPOP" | "BRPOP" | "BRPOPLPUSH" | "BLMOVE" | "BLMPOP" | "BZPOPMIN" | "BZPOPMAX"
        | "BZMPOP" | "WAIT" | "WAITAOF" => true,
        "XREAD" | "XREADGROUP" => argv.iter().skip(1).any(|a| a.eq_ignore_ascii_case("BLOCK")),
        _ => false,
    }
}

/// Why a blocking command is refused, with the form of it that returns at once.
fn blocking_reason(name: &str) -> String {
    let instead = match name {
        "BLPOP" => "Use LPOP instead.",
        "BRPOP" => "Use RPOP instead.",
        "BRPOPLPUSH" | "BLMOVE" => "Use LMOVE instead.",
        "BLMPOP" => "Use LMPOP instead.",
        "BZPOPMIN" => "Use ZPOPMIN instead.",
        "BZPOPMAX" => "Use ZPOPMAX instead.",
        "BZMPOP" => "Use ZMPOP instead.",
        "XREAD" | "XREADGROUP" => "Run it without BLOCK instead.",
        _ => "",
    };
    format!(
        "{name} is not available here: it waits on the server until something happens or its timeout ends, and it would do so on a connection shared by every other query to this server, so they would all wait with it. VibeLens stops waiting after a while but the server does not: the command would stay there and could still take the next element, for nobody. {instead}"
    )
    .trim_end()
    .to_owned()
}

/// Commands the console refuses because they need a dedicated streaming connection, would
/// corrupt state on a shared multiplexed connection, or would hold it up. Returns the
/// user-facing explanation.
pub(super) fn blocked_reason(argv: &[String]) -> Option<String> {
    let name = argv.first()?.to_ascii_uppercase();
    if is_blocking_command(argv) {
        return Some(blocking_reason(&name));
    }
    let sub = argv.get(1).map(|s| s.to_ascii_uppercase());
    let msg = match name.as_str() {
        "SELECT" => "SELECT is not available: each logical database has its own connection, and switching one \
                     would change the database for every other query on it. Switch database in the UI instead."
            .to_owned(),
        "MONITOR" => "MONITOR streams every command indefinitely and needs a dedicated connection; it is not supported here."
            .to_owned(),
        "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "UNSUBSCRIBE" | "PUNSUBSCRIBE" | "SUNSUBSCRIBE" => {
            format!("{name} needs a dedicated streaming connection (it puts the connection into pub/sub mode); it is not supported here.")
        }
        "MULTI" | "EXEC" | "DISCARD" | "WATCH" | "UNWATCH" => {
            format!("{name} is not supported: transactions are per-connection state and this connection is shared. Use a Lua script (EVAL) for atomic operations.")
        }
        "HELLO" | "AUTH" | "RESET" | "QUIT" => {
            format!("{name} would change the shared connection's protocol or identity and is managed by VibeLens.")
        }
        "SYNC" | "PSYNC" | "REPLCONF" => format!("{name} turns the connection into a replication stream; it is not supported here."),
        "CLIENT" if matches!(sub.as_deref(), Some("REPLY" | "TRACKING" | "CACHING" | "NO-TOUCH" | "SETNAME" | "SETINFO")) => {
            format!(
                "CLIENT {} changes state of the shared connection and is not supported here.",
                sub.unwrap_or_default()
            )
        }
        _ => return None,
    };
    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }
    fn simple(s: &str) -> Value {
        Value::SimpleString(s.into())
    }

    /// Mimics a Redis 7 `COMMAND INFO` entry (RESP3: flags are a set of simple strings).
    fn entry(name: &str, flags: &[&str], cats: &[&str], subs: usize) -> Value {
        Value::Array(vec![Value::Array(vec![
            bulk(name),
            Value::Int(-2),
            Value::Set(flags.iter().map(|f| simple(f)).collect()),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            Value::Set(cats.iter().map(|c| simple(c)).collect()),
            Value::Array(vec![]),
            Value::Array(vec![]),
            Value::Array((0..subs).map(|_| Value::Array(vec![])).collect()),
        ])])
    }

    fn classify(name: &str, sub: Option<&str>, reply: &Value) -> Classification {
        let meta = parse_command_info(reply).expect("meta");
        let display = match sub {
            Some(s) => format!("{} {}", name.to_uppercase(), s.to_uppercase()),
            None => name.to_uppercase(),
        };
        classify_with_meta(&display, name, sub, &meta)
    }

    #[test]
    fn parses_resp3_and_resp2_shapes() {
        let m = parse_command_info(&entry(
            "config|set",
            &["admin", "noscript"],
            &["@admin", "@dangerous"],
            0,
        ))
        .unwrap();
        assert_eq!(m.flags, ["admin", "noscript"]);
        assert_eq!(m.categories, ["@admin", "@dangerous"]);
        assert!(!m.has_subcommands);

        let parent = parse_command_info(&entry("config", &[], &["@slow"], 5)).unwrap();
        assert!(parent.has_subcommands);

        // RESP2 style: flags as arrays
        let resp2 = Value::Array(vec![Value::Array(vec![
            bulk("get"),
            Value::Int(2),
            Value::Array(vec![simple("readonly"), simple("fast")]),
        ])]);
        let m = parse_command_info(&resp2).unwrap();
        assert_eq!(m.flags, ["readonly", "fast"]);
        assert!(m.categories.is_empty());
    }

    #[test]
    fn unknown_command_is_none() {
        assert!(parse_command_info(&Value::Array(vec![Value::Nil])).is_none());
        assert!(parse_command_info(&Value::Array(vec![])).is_none());
        assert!(parse_command_info(&Value::Nil).is_none());
    }

    #[test]
    fn get_is_read_set_is_write() {
        assert!(
            !classify(
                "get",
                None,
                &entry("get", &["readonly", "fast"], &["@read"], 0)
            )
            .write
        );
        let set = classify(
            "set",
            None,
            &entry("set", &["write", "denyoom"], &["@write"], 0),
        );
        assert!(set.write);
        assert!(set.reason().contains("SET"));
        assert!(set.reason().contains("write, denyoom"));
    }

    #[test]
    fn config_get_vs_set_have_identical_server_flags_but_different_verdicts() {
        let flags = ["admin", "noscript", "loading", "stale"];
        let cats = ["@admin", "@slow", "@dangerous"];
        let get = classify(
            "config",
            Some("get"),
            &entry("config|get", &flags, &cats, 0),
        );
        let set = classify(
            "config",
            Some("set"),
            &entry("config|set", &flags, &cats, 0),
        );
        assert!(!get.write, "CONFIG GET must be a read");
        assert!(set.write, "CONFIG SET must be a write");
    }

    #[test]
    fn client_list_read_client_kill_write() {
        let flags = ["admin", "noscript", "loading", "stale"];
        assert!(
            !classify(
                "client",
                Some("list"),
                &entry("client|list", &flags, &["@admin"], 0)
            )
            .write
        );
        assert!(
            classify(
                "client",
                Some("kill"),
                &entry("client|kill", &flags, &["@admin"], 0)
            )
            .write
        );
    }

    #[test]
    fn flushall_and_eval_are_writes() {
        assert!(
            classify(
                "flushall",
                None,
                &entry("flushall", &["write"], &["@write", "@dangerous"], 0)
            )
            .write
        );
        let eval = [
            "noscript",
            "stale",
            "skip_monitor",
            "no_mandatory_keys",
            "movablekeys",
        ];
        assert!(
            classify(
                "eval",
                None,
                &entry("eval", &eval, &["@slow", "@scripting"], 0)
            )
            .write
        );
        assert!(
            classify(
                "fcall",
                None,
                &entry("fcall", &eval, &["@slow", "@scripting"], 0)
            )
            .write
        );
        assert!(
            !classify(
                "eval_ro",
                None,
                &entry("eval_ro", &eval, &["@slow", "@scripting"], 0)
            )
            .write
        );
    }

    #[test]
    fn script_and_function_subcommands() {
        let cats = ["@slow", "@scripting"];
        assert!(
            classify(
                "script",
                Some("flush"),
                &entry("script|flush", &["noscript"], &cats, 0)
            )
            .write
        );
        assert!(
            !classify(
                "script",
                Some("exists"),
                &entry("script|exists", &["noscript"], &cats, 0)
            )
            .write
        );
        assert!(
            classify(
                "function",
                Some("flush"),
                &entry("function|flush", &["noscript", "write"], &cats, 0)
            )
            .write
        );
        assert!(
            !classify(
                "function",
                Some("list"),
                &entry("function|list", &["noscript"], &cats, 0)
            )
            .write
        );
    }

    #[test]
    fn info_keys_scan_are_reads_even_though_dangerous() {
        assert!(
            !classify(
                "info",
                None,
                &entry("info", &["loading", "stale"], &["@slow", "@dangerous"], 0)
            )
            .write
        );
        assert!(
            !classify(
                "keys",
                None,
                &entry("keys", &["readonly"], &["@keyspace", "@dangerous"], 0)
            )
            .write
        );
        assert!(
            !classify(
                "scan",
                None,
                &entry("scan", &["readonly"], &["@keyspace"], 0)
            )
            .write
        );
    }

    #[test]
    fn pubsub_and_blocking() {
        assert!(
            classify(
                "publish",
                None,
                &entry("publish", &["pubsub", "fast"], &["@pubsub"], 0)
            )
            .write
        );
        assert!(
            !classify(
                "pubsub",
                Some("numsub"),
                &entry("pubsub|numsub", &["pubsub"], &["@pubsub"], 0)
            )
            .write
        );
        // blocking read: not listed as read, flagged blocking -> needs confirmation
        assert!(
            classify(
                "xread",
                None,
                &entry(
                    "xread",
                    &["readonly", "blocking"],
                    &["@read", "@blocking"],
                    0
                )
            )
            .write
        );
    }

    #[test]
    fn unknown_is_write() {
        let c = classify_unknown("FOO");
        assert!(c.write);
        assert!(c.reason().contains("FOO"));
    }

    #[test]
    fn fallback_table() {
        assert!(!classify_fallback("GET", "get", None).write);
        assert!(!classify_fallback("CONFIG GET", "config", Some("get")).write);
        assert!(classify_fallback("CONFIG SET", "config", Some("set")).write);
        assert!(classify_fallback("SET", "set", None).write);
        assert!(classify_fallback("FLUSHALL", "flushall", None).write);
        assert!(classify_fallback("EVAL", "eval", None).write);
        assert!(classify_fallback("CONFIG", "config", None).write);
        assert!(!classify_fallback("COMMAND", "command", None).write);
        // "get" as the *argument* of a non-parent command does not matter
        assert!(!classify_fallback("GET", "get", Some("config")).write);
        // "config set" must not be accidentally read via the plain-name entry
        assert!(!is_read_listed("config", Some("set")));
    }

    #[test]
    fn read_table_has_no_known_writers() {
        // Guard against someone adding a mutating command to the allow-list.
        for banned in [
            "set",
            "del",
            "flushall",
            "flushdb",
            "eval",
            "evalsha",
            "fcall",
            "config|set",
            "config|rewrite",
            "config|resetstat",
            "client|kill",
            "client|pause",
            "script|flush",
            "script|load",
            "function|load",
            "function|flush",
            "acl|setuser",
            "acl|deluser",
            "debug",
            "shutdown",
            "xread",
            "blpop",
            "publish",
            "monitor",
            "slowlog|reset",
            "memory|purge",
        ] {
            assert!(
                !READ_ONLY.contains(&banned),
                "{banned} must not be on the read-only list"
            );
        }
    }

    #[test]
    fn blocked_commands() {
        let b = |a: &[&str]| blocked_reason(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(
            b(&["select", "3"])
                .unwrap()
                .contains("Switch database in the UI")
        );
        assert!(b(&["MONITOR"]).is_some());
        assert!(b(&["subscribe", "ch"]).is_some());
        assert!(b(&["PSUBSCRIBE", "a*"]).is_some());
        assert!(b(&["ssubscribe", "ch"]).is_some());
        assert!(b(&["multi"]).is_some());
        assert!(b(&["hello", "2"]).is_some());
        assert!(b(&["client", "reply", "off"]).is_some());
        assert!(b(&["client", "list"]).is_none());
        assert!(b(&["get", "select"]).is_none());
        assert!(b(&["set", "k", "v"]).is_none());
        assert!(b(&[]).is_none());
    }

    /// A blocking command waits on the server for as long as it likes. On a connection every
    /// other query shares, that stalls all of them; and when the client gives up waiting, the
    /// server does not: the command can still take an element later, for nobody (finding M9).
    #[test]
    fn blocking_commands_are_refused_with_the_reason_and_the_form_that_does_not_block() {
        let b = |a: &[&str]| blocked_reason(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        for (argv, instead) in [
            (&["BLPOP", "jobs", "0"][..], "LPOP"),
            (&["brpop", "jobs", "5"], "RPOP"),
            (&["BRPOPLPUSH", "a", "b", "0"], "LMOVE"),
            (&["BLMOVE", "a", "b", "LEFT", "RIGHT", "0"], "LMOVE"),
            (&["BLMPOP", "0", "1", "a", "LEFT"], "LMPOP"),
            (&["BZPOPMIN", "z", "0"], "ZPOPMIN"),
            (&["BZPOPMAX", "z", "0"], "ZPOPMAX"),
            (&["BZMPOP", "0", "1", "z", "MIN"], "ZMPOP"),
            (
                &["XREAD", "BLOCK", "0", "STREAMS", "s", "$"],
                "without BLOCK",
            ),
            (
                &[
                    "xreadgroup",
                    "GROUP",
                    "g",
                    "c",
                    "block",
                    "100",
                    "STREAMS",
                    "s",
                    ">",
                ],
                "without BLOCK",
            ),
        ] {
            let msg = b(argv).unwrap_or_else(|| panic!("{argv:?} must be refused"));
            assert!(
                msg.contains(&argv[0].to_ascii_uppercase())
                    && msg.contains("shared")
                    && msg.contains(instead),
                "{argv:?}: {msg}"
            );
        }
        for argv in [&["WAIT", "1", "0"][..], &["waitaof", "1", "0", "0"]] {
            let msg = b(argv).unwrap_or_else(|| panic!("{argv:?} must be refused"));
            assert!(msg.contains("shared"), "{argv:?}: {msg}");
        }
        // The forms that do not block are ordinary commands.
        for argv in [
            &["XREAD", "COUNT", "10", "STREAMS", "s", "0"][..],
            &["XREADGROUP", "GROUP", "g", "c", "STREAMS", "s", ">"],
            &["LPOP", "jobs"],
            &["LMOVE", "a", "b", "LEFT", "RIGHT"],
            &["ZPOPMIN", "z"],
            &["GET", "blpop"],
        ] {
            assert!(b(argv).is_none(), "{argv:?}");
        }
        // One list for the console and the agent.
        let listed =
            |a: &[&str]| is_blocking_command(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(
            listed(&["blpop", "k", "0"]) && listed(&["XREAD", "block", "0", "STREAMS", "s", "$"])
        );
        assert!(
            !listed(&["XREAD", "STREAMS", "s", "0"]) && !listed(&["LPOP", "k"]) && !listed(&[])
        );
    }
}
