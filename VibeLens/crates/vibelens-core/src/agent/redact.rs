//! What the model is allowed to learn from a failed tool call.
//!
//! Tool errors travel to the LLM provider as tool results. Two things must not leak that way:
//!
//! * **Where we connect and who as**: tunnel hosts, IP addresses, `user@host`, `for user "x"`.
//!   Connection-level failures (tunnel, authentication, AWS, vault, not-connected) are therefore
//!   replaced wholesale by fixed sentences; every other message is scrubbed of those patterns.
//!   The UI's `ToolFinished` summary keeps the original text, which never leaves the machine.
//! * **Row values under a policy that withholds them** (`schemaOnly`, or an approved write on a
//!   `schemaOnly` connection). PostgreSQL puts values in `DETAIL`/`WHERE` lines and inside
//!   messages (`invalid input syntax for type integer: "secret"`); Valkey echoes arguments.
//!   Database and Valkey errors are reduced to a SQLSTATE / error code plus a fixed sentence.
//!
//! The pg error text carries no SQLSTATE (see `pg/error.rs`: message, position, DETAIL, HINT),
//! so the code is recovered from the standard server message wording, or from a literal
//! `SQLSTATE xxxxx` if one is ever added to the text.

use crate::error::CoreError;
use crate::model::EngineKind;

/// Text of `e` that is safe to send to the model. `withhold_values` is true when the call ran
/// under a policy that keeps data values away from the model. `engine` is the connection the
/// call targeted, when there was one.
///
/// Iceberg engines (DataFusion, Athena) put offending values in their messages too (`Cannot cast
/// string 'alice' to Int64`, Parquet decode errors) and we do not control which error variant they
/// arrive in, so under a withholding policy every engine-side failure collapses to a fixed sentence.
pub(super) fn model_error_text(
    e: &CoreError,
    withhold_values: bool,
    engine: Option<EngineKind>,
) -> String {
    match e {
        CoreError::Tunnel(_) => "connection failed: tunnel error".into(),
        CoreError::Auth(_) => {
            "authentication failed; ask the user to check this connection's credentials".into()
        }
        CoreError::NotConnected(_) => {
            "not connected: the connection is unavailable; ask the user to reconnect".into()
        }
        CoreError::Aws(_) => "AWS request failed".into(),
        CoreError::Vault(_) => "credential store error".into(),
        CoreError::Database(_) | CoreError::Other(_) | CoreError::Io(_) | CoreError::Serde(_)
            if withhold_values && engine == Some(EngineKind::Iceberg) =>
        {
            "the Iceberg engine reported an error; details are withheld because this connection does not share data values with the model".into()
        }
        CoreError::Database(m) if withhold_values => generic_db_error(m),
        CoreError::Valkey(m) if withhold_values => generic_valkey_error(m),
        other => scrub_sensitive(&other.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// Database errors without values
// ---------------------------------------------------------------------------------------------

/// First `SQLSTATE` code literally present in the text (`SQLSTATE 23505`, `[SQLSTATE=23505]`).
fn literal_sqlstate(msg: &str) -> Option<String> {
    let at = msg.find("SQLSTATE")? + "SQLSTATE".len();
    let code: String = msg[at..]
        .trim_start_matches([' ', ':', '=', '['])
        .chars()
        .take(5)
        .collect();
    (code.len() == 5 && code.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| code.to_ascii_uppercase())
}

/// SQLSTATE for the standard server message wording (first line, lower-cased).
fn sqlstate_from_message(line: &str) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("duplicate key value", "23505"),
        ("violates foreign key constraint", "23503"),
        ("violates not-null constraint", "23502"),
        ("violates check constraint", "23514"),
        ("violates exclusion constraint", "23P01"),
        ("conflicting key value violates exclusion", "23P01"),
        ("invalid input syntax", "22P02"),
        ("invalid input value for enum", "22P02"),
        ("invalid text representation", "22P02"),
        ("value too long", "22001"),
        ("out of range", "22003"),
        ("division by zero", "22012"),
        ("invalid datetime format", "22007"),
        ("date/time field value out of range", "22008"),
        ("syntax error", "42601"),
        ("permission denied", "42501"),
        ("must be owner", "42501"),
        ("relation", "42P01"),
        ("column", "42703"),
        ("function", "42883"),
        ("could not serialize", "40001"),
        ("deadlock detected", "40P01"),
        ("read-only transaction", "25006"),
        ("statement timeout", "57014"),
        ("canceling statement", "57014"),
        ("could not obtain lock", "55P03"),
        ("lock timeout", "55P03"),
    ];
    let relation_like = line.contains("does not exist");
    TABLE
        .iter()
        .find(|(needle, code)| {
            line.contains(needle)
                // "relation"/"column"/"function" are only meaningful in "… does not exist".
                && (!matches!(*code, "42P01" | "42703" | "42883") || relation_like)
        })
        .map(|(_, code)| *code)
}

fn sqlstate_sentence(code: &str) -> &'static str {
    match code {
        "23505" => "duplicate key value violates a unique constraint",
        "23503" => "violates a foreign key constraint",
        "23502" => "null value violates a not-null constraint",
        "23514" => "violates a check constraint",
        "23P01" => "violates an exclusion constraint",
        "22P02" => "invalid input syntax",
        "22001" => "value too long for the column type",
        "22003" => "numeric value out of range",
        "22007" | "22008" => "invalid date or time value",
        "22012" => "division by zero",
        "42601" => "syntax error",
        "42501" => "permission denied",
        "42P01" => "relation does not exist",
        "42703" => "column does not exist",
        "42883" => "function does not exist",
        "40001" => "could not serialize access due to concurrent update",
        "40P01" => "deadlock detected",
        "25006" => "cannot execute in a read-only transaction",
        "57014" => "query canceled or timed out",
        "55P03" => "lock not available",
        c if c.starts_with("22") => "data exception",
        c if c.starts_with("23") => "integrity constraint violation",
        c if c.starts_with("42") => "syntax error or access rule violation",
        c if c.starts_with("40") => "transaction rolled back",
        _ => "database error",
    }
}

/// `SQLSTATE <code>: <generic message>`, with no DETAIL, HINT, WHERE, positions or quoted
/// values. Unrecognised errors become plain `database error`.
pub(super) fn generic_db_error(msg: &str) -> String {
    let code = literal_sqlstate(msg).or_else(|| {
        let first = msg.lines().next().unwrap_or("").to_ascii_lowercase();
        sqlstate_from_message(&first).map(str::to_string)
    });
    match code {
        Some(c) => format!("SQLSTATE {c}: {}", sqlstate_sentence(&c)),
        None => "database error".to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Valkey errors without values
// ---------------------------------------------------------------------------------------------

/// `CODE: <generic message>` for a Valkey error (`WRONGTYPE …`, `ERR …`); everything after the
/// code is dropped because it can echo keys, arguments or script output.
pub(super) fn generic_valkey_error(msg: &str) -> String {
    let code = msg
        .split_whitespace()
        .take(6)
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .find(|t| {
            (3..=24).contains(&t.len())
                && t.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && t.chars().any(|c| c.is_ascii_uppercase())
        });
    let Some(code) = code else {
        return "valkey error".to_string();
    };
    let sentence = match code {
        "WRONGTYPE" => "operation against a key holding the wrong kind of value",
        "NOSCRIPT" => "script not found",
        "BUSY" | "BUSYKEY" => "server is busy",
        "OOM" => "server is out of memory",
        "NOPERM" => "permission denied for this command or key",
        "READONLY" => "server is read-only",
        "LOADING" => "server is loading its dataset",
        "EXECABORT" => "transaction aborted",
        "MOVED" | "ASK" | "CLUSTERDOWN" | "CROSSSLOT" | "TRYAGAIN" => "cluster routing error",
        "NOAUTH" | "WRONGPASS" => "authentication required",
        "ERR" => "command failed",
        _ => "command failed",
    };
    format!("{code}: {sentence}")
}

// ---------------------------------------------------------------------------------------------
// Scrubbing connection details
// ---------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Seg<'a> {
    Space(&'a str),
    Word(&'a str),
    /// `"…"` or `'…'`, quotes included (unterminated runs to the end).
    Quoted(&'a str),
}

fn segments(s: &str) -> Vec<Seg<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        if c.is_ascii_whitespace() {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(Seg::Space(&s[start..i]));
        } else if c == b'"' || c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != c {
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push(Seg::Quoted(&s[start..i]));
        } else {
            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'"' && b[i] != b'\'' {
                i += 1;
            }
            out.push(Seg::Word(&s[start..i]));
        }
    }
    out
}

fn is_ipv4(s: &str) -> bool {
    let host = s.rsplit_once(':').map_or(s, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() {
            h
        } else {
            s
        }
    });
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
}

fn is_ipv6(s: &str) -> bool {
    (s.contains("::") || s.starts_with('['))
        && s.contains(':')
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '[' | ']'))
}

fn is_host_port(s: &str) -> bool {
    let Some((host, port)) = s.rsplit_once(':') else {
        return false;
    };
    (2..=5).contains(&port.len())
        && port.chars().all(|c| c.is_ascii_digit())
        && !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && host.chars().any(|c| c.is_ascii_alphabetic() || c == '.')
}

/// Replacement for a word that looks like an address or a user, if it does.
fn sensitive_word(core: &str) -> Option<&'static str> {
    if let Some((l, r)) = core.split_once('@')
        && !l.is_empty()
        && !r.is_empty()
    {
        return Some("<redacted>");
    }
    if is_ipv4(core) || is_ipv6(core) {
        return Some("<ip>");
    }
    if is_host_port(core) {
        return Some("<host>");
    }
    None
}

const LEAD: &[char] = &['(', '<', ',', ';'];
const TRAIL: &[char] = &[')', '>', ',', ';', '.', ':'];

/// Removes hostnames, addresses and user names from free-form error text: `user@host`, IPv4 and
/// IPv6 addresses, `host:port`, and the name after `user`/`role`/`host` (`for user "x"`,
/// `role "x" does not exist`). Conservative: ordinary SQL and identifiers are left alone.
pub(super) fn scrub_sensitive(text: &str) -> String {
    let segs = segments(text);
    let mut out = String::with_capacity(text.len());
    // The last two non-space words, lower-cased and stripped of punctuation.
    let mut prev: Option<String> = None;
    let mut prev2: Option<String> = None;
    let trigger = |p: &Option<String>| {
        matches!(
            p.as_deref(),
            Some("user" | "role" | "username" | "host" | "hostname" | "server")
        )
    };
    for seg in segs {
        match seg {
            Seg::Space(s) => out.push_str(s),
            Seg::Word(w) => {
                let body = w.trim_start_matches(LEAD);
                let lead = &w[..w.len() - body.len()];
                let core = body.trim_end_matches(TRAIL);
                let trail = &body[core.len()..];
                let after_user_for = trigger(&prev) && prev2.as_deref() == Some("for");
                let replacement = sensitive_word(core).or_else(|| {
                    // `for user bob`, `host db1`: the bare word after the trigger is the name.
                    let hostish = matches!(prev.as_deref(), Some("host" | "hostname"));
                    ((after_user_for || hostish) && !core.is_empty()).then_some("<redacted>")
                });
                out.push_str(lead);
                out.push_str(replacement.unwrap_or(core));
                out.push_str(trail);
                prev2 = prev.take();
                prev = Some(core.to_ascii_lowercase());
            }
            Seg::Quoted(q) => {
                let quote = &q[..1];
                let closed = q.len() >= 2 && q.ends_with(quote);
                let inner = &q[1..q.len() - usize::from(closed)];
                let replacement = if trigger(&prev) {
                    Some("<redacted>")
                } else {
                    sensitive_word(inner)
                };
                out.push_str(quote);
                out.push_str(replacement.unwrap_or(inner));
                if closed {
                    out.push_str(quote);
                }
                prev2 = prev.take();
                prev = Some(String::new());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(e: CoreError, withhold: bool) -> String {
        model_error_text(&e, withhold, None)
    }

    #[test]
    fn iceberg_engine_errors_carry_no_text_under_a_withholding_policy() {
        let leaky = "Arrow error: Cast error: Cannot cast string 'alice@bank.example' to value of Int64 type";
        for e in [
            CoreError::Database(leaky.into()),
            CoreError::Other(leaky.into()),
            CoreError::Io(std::io::Error::other(leaky)),
        ] {
            let t = model_error_text(&e, true, Some(EngineKind::Iceberg));
            assert!(!t.contains("alice") && !t.contains("Cast"), "{t}");
            assert!(t.contains("withheld"), "{t}");
        }
        // Our own validation messages still reach the model, and so do errors when values are shared.
        let t = model_error_text(
            &CoreError::Invalid("only queries are allowed in read-only mode".into()),
            true,
            Some(EngineKind::Iceberg),
        );
        assert!(t.contains("only queries"), "{t}");
        let t = model_error_text(
            &CoreError::Database(leaky.into()),
            false,
            Some(EngineKind::Iceberg),
        );
        assert!(t.contains("Cannot cast"), "{t}");
        // Postgres keeps its SQLSTATE treatment.
        let t = model_error_text(
            &CoreError::Database("duplicate key value violates unique constraint \"k\"".into()),
            true,
            Some(EngineKind::Postgres),
        );
        assert!(t.starts_with("SQLSTATE 23505"), "{t}");
    }

    #[test]
    fn connection_errors_become_fixed_sentences() {
        let t = text(
            CoreError::Tunnel("cannot reach SSH host bastion.corp.example.com:22: refused".into()),
            false,
        );
        assert_eq!(t, "connection failed: tunnel error");
        let t = text(
            CoreError::Auth("rejected the offered key for user \"deploy\"".into()),
            false,
        );
        assert!(t.starts_with("authentication failed"));
        assert!(!t.contains("deploy"));
        let t = text(
            CoreError::NotConnected("db.internal:5432 closed the connection".into()),
            true,
        );
        assert!(t.starts_with("not connected"));
        assert!(!t.contains("internal"));
        assert_eq!(
            text(
                CoreError::Aws("sts.eu-west-1: arn:aws:iam::123:role/x denied".into()),
                false
            ),
            "AWS request failed"
        );
        assert_eq!(
            text(CoreError::Vault("keyring entry bob@prod".into()), false),
            "credential store error"
        );
    }

    #[test]
    fn free_text_errors_are_scrubbed_not_replaced() {
        let t = text(
            CoreError::Database(
                "error connecting to server: could not connect to 10.0.3.7:5432".into(),
            ),
            false,
        );
        assert_eq!(
            t,
            "database: error connecting to server: could not connect to <ip>"
        );
        let t = text(
            CoreError::Database("FATAL: role \"app_user\" does not exist".into()),
            false,
        );
        assert_eq!(t, "database: FATAL: role \"<redacted>\" does not exist");
        // SQL problems stay legible when values are not withheld.
        let t = text(
            CoreError::Database("relation \"public.users\" does not exist (position 15)".into()),
            false,
        );
        assert_eq!(
            t,
            "database: relation \"public.users\" does not exist (position 15)"
        );
        let t = text(
            CoreError::Timeout("get_schema did not finish within 180 seconds".into()),
            false,
        );
        assert_eq!(t, "timed out: get_schema did not finish within 180 seconds");
    }

    #[test]
    fn scrub_patterns() {
        for (input, expect) in [
            (
                "password authentication failed for user \"bob\"",
                "password authentication failed for user \"<redacted>\"",
            ),
            (
                "no pg_hba.conf entry for host \"203.0.113.9\", user \"bob\", database \"d\"",
                "no pg_hba.conf entry for host \"<redacted>\", user \"<redacted>\", database \"d\"",
            ),
            (
                "login as alice@db.example.com failed",
                "login as <redacted> failed",
            ),
            ("connect to [::1]:5432 failed", "connect to <ip> failed"),
            ("connect to fe80::1 failed", "connect to <ip> failed"),
            (
                "cannot reach host db1.corp:2222",
                "cannot reach host <host>",
            ),
            ("dial cache.internal:6379: refused", "dial <host>: refused"),
            (
                "rejected key for user bob",
                "rejected key for user <redacted>",
            ),
            (
                "user \"john smith\" locked out",
                "user \"<redacted>\" locked out",
            ),
            ("unterminated user \"bob", "unterminated user \"<redacted>"),
        ] {
            assert_eq!(scrub_sensitive(input), expect, "{input}");
        }
        // Ordinary text is left alone.
        for ok in [
            "syntax error at or near \"from\" (position 8)",
            "select 1:30 time, 12:30:45",
            "column \"users.id\" must appear in the GROUP BY clause",
            "permission denied for table users",
            "user-defined function raised an exception",
            "cannot execute INSERT in a read-only transaction",
            "",
        ] {
            assert_eq!(scrub_sensitive(ok), ok, "{ok}");
        }
    }

    /// Reviewer finding: schemaOnly connections leaked row values through error text.
    #[test]
    fn database_errors_are_generic_when_values_are_withheld() {
        let cases = [
            (
                "duplicate key value violates unique constraint \"users_email_key\"\nDETAIL: Key (email)=(ceo@bank.example) already exists.",
                "SQLSTATE 23505: duplicate key value violates a unique constraint",
            ),
            (
                "null value in column \"ssn\" of relation \"people\" violates not-null constraint\nDETAIL: Failing row contains (1, Alice, null).",
                "SQLSTATE 23502: null value violates a not-null constraint",
            ),
            (
                "new row for relation \"acct\" violates check constraint \"bal_pos\"\nDETAIL: Failing row contains (7, -100).",
                "SQLSTATE 23514: violates a check constraint",
            ),
            (
                "insert or update on table \"o\" violates foreign key constraint \"o_u_fk\"\nDETAIL: Key (user_id)=(99) is not present in table \"users\".",
                "SQLSTATE 23503: violates a foreign key constraint",
            ),
            (
                "invalid input syntax for type integer: \"hunter2\" (position 30)",
                "SQLSTATE 22P02: invalid input syntax",
            ),
            (
                "invalid input value for enum mood: \"secret\"",
                "SQLSTATE 22P02: invalid input syntax",
            ),
            (
                "value too long for type character varying(5)",
                "SQLSTATE 22001: value too long for the column type",
            ),
            ("division by zero", "SQLSTATE 22012: division by zero"),
            (
                "syntax error at or near \"4111111111111111\" (position 12)",
                "SQLSTATE 42601: syntax error",
            ),
            (
                "relation \"secret_table\" does not exist",
                "SQLSTATE 42P01: relation does not exist",
            ),
            (
                "column \"pin\" does not exist\nHINT: Perhaps you meant \"pin_hash\".",
                "SQLSTATE 42703: column does not exist",
            ),
            (
                "permission denied for table salaries",
                "SQLSTATE 42501: permission denied",
            ),
            (
                "canceling statement due to statement timeout",
                "SQLSTATE 57014: query canceled or timed out",
            ),
            (
                "cannot execute DELETE in a read-only transaction",
                "SQLSTATE 25006: cannot execute in a read-only transaction",
            ),
            // A literal SQLSTATE wins; unknown classes still get a class-level sentence.
            (
                "boom [SQLSTATE 23xyz] Key (a)=(secret)",
                "SQLSTATE 23XYZ: integrity constraint violation",
            ),
            (
                "SQLSTATE=22023 DETAIL: x=secret",
                "SQLSTATE 22023: data exception",
            ),
            // Unrecognised text is not echoed at all.
            ("something odd with value 'top secret'", "database error"),
            ("", "database error"),
        ];
        for (input, expect) in cases {
            let got = text(CoreError::Database(input.into()), true);
            assert_eq!(got, expect, "{input}");
            assert!(!got.contains("secret") && !got.contains("DETAIL"), "{got}");
        }
        // Unredacted mode keeps the real message.
        let raw = "duplicate key value violates unique constraint \"k\"\nDETAIL: Key (a)=(1) already exists.";
        assert!(text(CoreError::Database(raw.into()), false).contains("Key (a)=(1)"));
    }

    #[test]
    fn valkey_errors_are_generic_when_values_are_withheld() {
        for (input, expect) in [
            (
                "WRONGTYPE Operation against a key holding the wrong kind of value",
                "WRONGTYPE: operation against a key holding the wrong kind of value",
            ),
            (
                "ERR unknown command 'password123', with args beginning with: 'hunter2'",
                "ERR: command failed",
            ),
            (
                "ERR user_script:1: Script attempted to access nonexistent global variable 'ssn'",
                "ERR: command failed",
            ),
            ("NOSCRIPT No matching script", "NOSCRIPT: script not found"),
            ("MOVED 3999 10.0.0.5:6381", "MOVED: cluster routing error"),
            (
                "BUSY Redis is busy running a script",
                "BUSY: server is busy",
            ),
            (
                "EXECABORT Transaction discarded",
                "EXECABORT: transaction aborted",
            ),
            ("custom failure with payload 'secret'", "valkey error"),
            ("", "valkey error"),
        ] {
            let got = text(CoreError::Valkey(input.into()), true);
            assert_eq!(got, expect, "{input}");
            assert!(!got.contains("secret") && !got.contains("hunter2") && !got.contains("10.0"));
        }
        // Not withheld: the message passes through (scrubbed of addresses only).
        let t = text(
            CoreError::Valkey("ERR value is not an integer".into()),
            false,
        );
        assert_eq!(t, "valkey: ERR value is not an integer");
        let t = text(CoreError::Valkey("MOVED 3999 10.0.0.5:6381".into()), false);
        assert_eq!(t, "valkey: MOVED 3999 <ip>");
    }

    #[test]
    fn segments_cover_the_whole_input() {
        for s in ["", " ", "a b", "a \"b c\" d", "x 'y", "\"", "a\t\nb"] {
            let joined: String = segments(s)
                .iter()
                .map(|g| match g {
                    Seg::Space(x) | Seg::Word(x) | Seg::Quoted(x) => *x,
                })
                .collect();
            assert_eq!(joined, s);
        }
    }
}
