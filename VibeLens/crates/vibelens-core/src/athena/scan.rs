//! A tiny SQL word scanner for two jobs: the read-only fallback guard and command tags.
//!
//! `crate::sql::classify` parses with the PostgreSQL dialect, which rejects Athena (Trino)
//! syntax such as `FOR TIMESTAMP AS OF`, `SHOW TABLES` or `DESCRIBE`. Those statements classify
//! as "not provably read-only" and stay denied unless [`provably_read_only`] vouches for them.
//! That fallback is deliberately narrower than the classifier: it never admits comments,
//! semicolons or unbalanced quotes, and it only admits statements that start with a keyword
//! that cannot begin a write in Athena's grammar.

/// Words scanned out of a statement (upper-cased), ignoring string literals and quoted
/// identifiers, plus flags for constructs the fallback refuses to reason about.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    pub words: Vec<String>,
    pub has_comment: bool,
    pub has_semicolon: bool,
    /// False when a quote or comment was left open.
    pub balanced: bool,
}

pub(crate) fn scan(sql: &str) -> Scan {
    let b = sql.as_bytes();
    let mut out = Scan {
        balanced: true,
        ..Scan::default()
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                out.has_comment = true;
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.has_comment = true;
                i += 2;
                loop {
                    if i + 1 >= b.len() {
                        out.balanced = false;
                        i = b.len();
                        break;
                    }
                    if b[i] == b'*' && b[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            b'\'' | b'"' | b'`' => {
                i += 1;
                loop {
                    match b.get(i) {
                        None => {
                            out.balanced = false;
                            break;
                        }
                        Some(&q) if q == c => {
                            if b.get(i + 1) == Some(&c) {
                                i += 2;
                            } else {
                                i += 1;
                                break;
                            }
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            b';' => {
                out.has_semicolon = true;
                i += 1;
            }
            _ if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 => {
                let start = i;
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'_'
                        || b[i] == b'$'
                        || b[i] >= 0x80)
                {
                    i += 1;
                }
                // `start` and `i` sit on ASCII boundaries or inside multi-byte runs consumed whole.
                out.words
                    .push(String::from_utf8_lossy(&b[start..i]).to_ascii_uppercase());
            }
            _ => i += 1,
        }
    }
    out
}

/// Words that start (or smuggle in) a statement that changes data, metadata or permissions.
/// Applied to `SELECT` / `WITH` text as belt and braces; quoted identifiers are not scanned.
const WRITE_WORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "DROP", "ALTER", "TRUNCATE", "UNLOAD", "CALL",
    "GRANT", "REVOKE", "MSCK", "EXECUTE", "OPTIMIZE", "VACUUM", "INTO",
];

/// Conservative text-level check for statements `sqlparser` could not parse. True only for
/// `SELECT` / `WITH` queries free of write keywords, and for `SHOW` / `DESCRIBE`, with no
/// comments, semicolons or unbalanced quotes. Anything else is not vouched for.
pub(crate) fn provably_read_only(stmt: &str) -> bool {
    let s = scan(stmt);
    if s.has_comment || s.has_semicolon || !s.balanced {
        return false;
    }
    match s.words.first().map(String::as_str) {
        Some("SHOW" | "DESCRIBE") => true,
        Some("SELECT" | "WITH") => !s.words.iter().any(|w| WRITE_WORDS.contains(&w.as_str())),
        _ => false,
    }
}

/// Whether the statement starts with `SHOW` or `DESCRIBE`.
pub(crate) fn is_show_or_describe(stmt: &str) -> bool {
    matches!(
        scan(stmt).words.first().map(String::as_str),
        Some("SHOW" | "DESCRIBE")
    )
}

/// A short command tag for the statement: `SELECT`, `INSERT`, `CREATE TABLE`, `DROP VIEW`, …
pub(crate) fn command_tag(stmt: &str) -> Option<String> {
    let s = scan(stmt);
    let first = s.words.first()?.as_str();
    match first {
        "CREATE" | "DROP" | "ALTER" => {
            const NOISE: &[&str] = &[
                "OR",
                "REPLACE",
                "EXTERNAL",
                "TEMPORARY",
                "TEMP",
                "MATERIALIZED",
                "GLOBAL",
                "IF",
                "NOT",
                "EXISTS",
            ];
            let object = s.words[1..].iter().find(|w| !NOISE.contains(&w.as_str()));
            Some(match object {
                Some(o) => format!("{first} {o}"),
                None => first.to_string(),
            })
        }
        "WITH" => Some("SELECT".to_string()),
        other => Some(other.to_string()),
    }
}

/// Whether the statement is a DML write that Athena reports through `UpdateCount`.
pub(crate) fn is_row_write(stmt: &str) -> bool {
    matches!(
        scan(stmt).words.first().map(String::as_str),
        Some("INSERT" | "UPDATE" | "DELETE" | "MERGE")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_admits_trino_reads_the_postgres_parser_cannot_parse() {
        for s in [
            "SHOW TABLES",
            "SHOW CREATE TABLE shop.orders",
            "DESCRIBE shop.orders",
            "describe \"shop\".\"orders$snapshots\"",
            "SELECT * FROM shop.orders FOR TIMESTAMP AS OF TIMESTAMP '2024-01-01 00:00:00 UTC'",
            "WITH a AS (SELECT 1) SELECT * FROM a",
            "SELECT \"update\", comment FROM t WHERE note = 'drop table x'",
        ] {
            assert!(provably_read_only(s), "{s}");
        }
    }

    #[test]
    fn fallback_refuses_everything_that_could_write() {
        for s in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "CREATE TABLE t AS SELECT 1",
            "DROP TABLE t",
            "ALTER TABLE t ADD COLUMNS (x int)",
            "OPTIMIZE t REWRITE DATA USING BIN_PACK",
            "VACUUM t",
            "UNLOAD (SELECT 1) TO 's3://b/p/' WITH (format = 'PARQUET')",
            "MSCK REPAIR TABLE t",
            "CALL system.rollback_to_snapshot('a', 't', 1)",
            "SELECT * INTO x FROM t",
            "SELECT 1; DROP TABLE t",
            "SELECT 1 -- DROP TABLE t",
            "SELECT 1 /* x */",
            "SELECT 'unterminated",
            "WITH a AS (SELECT 1) INSERT INTO t SELECT * FROM a",
            "EXPLAIN ANALYZE INSERT INTO t SELECT 1",
            "USING EXTERNAL FUNCTION f(x int) RETURNS int LAMBDA 'arn' SELECT f(1)",
            "",
            "   ",
        ] {
            assert!(!provably_read_only(s), "{s}");
        }
    }

    #[test]
    fn scan_ignores_literals_and_quoted_identifiers() {
        let s = scan("SELECT 'it''s; -- not a comment', \"a;b\" FROM `t`");
        assert!(s.balanced && !s.has_comment && !s.has_semicolon);
        assert_eq!(s.words, vec!["SELECT", "FROM"]);
    }

    #[test]
    fn command_tags() {
        assert_eq!(command_tag("select 1").as_deref(), Some("SELECT"));
        assert_eq!(
            command_tag("WITH a AS (SELECT 1) SELECT * FROM a").as_deref(),
            Some("SELECT")
        );
        assert_eq!(
            command_tag("insert into t values (1)").as_deref(),
            Some("INSERT")
        );
        assert_eq!(
            command_tag("CREATE TABLE t (a int)").as_deref(),
            Some("CREATE TABLE")
        );
        assert_eq!(
            command_tag("create or replace view v as select 1").as_deref(),
            Some("CREATE VIEW")
        );
        assert_eq!(
            command_tag("DROP TABLE IF EXISTS t").as_deref(),
            Some("DROP TABLE")
        );
        assert_eq!(
            command_tag("CREATE EXTERNAL TABLE t (a int)").as_deref(),
            Some("CREATE TABLE")
        );
        assert_eq!(command_tag("-- hi\nSHOW TABLES").as_deref(), Some("SHOW"));
        assert_eq!(command_tag("").as_deref(), None);
    }

    #[test]
    fn row_writes() {
        assert!(is_row_write("INSERT INTO t SELECT 1"));
        assert!(is_row_write("/* c */ delete from t"));
        assert!(!is_row_write("SELECT 1"));
        assert!(!is_row_write("CREATE TABLE t AS SELECT 1"));
    }
}
