//! What may be submitted to Redshift, decided before any request is made.
//!
//! The Data API runs every statement in its own session and commits it on its own, and it has
//! no way to make a session read-only. So two checks run over the whole script first:
//!
//! * **Transaction control is refused in every mode.** `BEGIN … COMMIT` would look atomic and
//!   not be: each statement has already committed by the time the next one starts.
//! * **A read-only run submits only statements that are provably reads (R5).** The
//!   PostgreSQL-dialect classifier must parse a statement and class it read-only; Redshift's
//!   `SHOW …` forms that the parser does not know are admitted by a narrow text check. Anything
//!   else, including text that does not parse, is refused.

use crate::error::{CoreError, Result};
use crate::sql::{StatementClass, classify};

/// Words of `stmt`, upper-cased, outside comments, string literals and quoted identifiers, plus
/// whether the text has anything the narrow check refuses to reason about.
struct Words {
    words: Vec<String>,
    has_comment: bool,
    has_semicolon: bool,
    has_dollar_quote: bool,
    balanced: bool,
}

fn words(sql: &str) -> Words {
    let b = sql.as_bytes();
    let mut out = Words {
        words: Vec::new(),
        has_comment: false,
        has_semicolon: false,
        has_dollar_quote: false,
        balanced: true,
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
                // Block comments nest in PostgreSQL and Redshift.
                let mut depth = 1;
                i += 2;
                while depth > 0 {
                    if i + 1 >= b.len() {
                        out.balanced = false;
                        i = b.len();
                        break;
                    }
                    if b[i] == b'/' && b[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'\'' | b'"' => {
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
                        // `\'` inside a literal: Redshift accepts backslash escapes.
                        Some(b'\\') if c == b'\'' => i += 2,
                        Some(_) => i += 1,
                    }
                }
            }
            b'$' => {
                out.has_dollar_quote = true;
                i += 1;
            }
            b';' => {
                out.has_semicolon = true;
                i += 1;
            }
            _ if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 => {
                let start = i;
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'_'
                        || b[i] == b'$'
                        || b[i] >= 0x80)
                {
                    i += 1;
                }
                out.words
                    .push(String::from_utf8_lossy(&b[start..i]).to_ascii_uppercase());
            }
            _ => i += 1,
        }
    }
    out
}

/// The transaction-control keyword `stmt` starts with, if any: `BEGIN`, `START TRANSACTION`,
/// `COMMIT`, `END`, `ROLLBACK`, `ABORT`, and the savepoint statements that only make sense
/// inside a transaction.
fn transaction_control(stmt: &str) -> Option<String> {
    let w = words(stmt).words;
    let first = w.first()?.as_str();
    match first {
        "BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "SAVEPOINT" => Some(first.to_string()),
        "START" if w.get(1).is_some_and(|x| x == "TRANSACTION") => {
            Some("START TRANSACTION".to_string())
        }
        "RELEASE" if w.get(1).is_some_and(|x| x == "SAVEPOINT") => {
            Some("RELEASE SAVEPOINT".to_string())
        }
        _ => None,
    }
}

fn which(i: usize, n: usize) -> String {
    if n > 1 {
        format!("statement {} of {n}: ", i + 1)
    } else {
        String::new()
    }
}

/// Refuses a script that contains transaction control, in every mode.
pub(crate) fn check_no_transaction_control(statements: &[String]) -> Result<()> {
    for (i, stmt) in statements.iter().enumerate() {
        if let Some(kw) = transaction_control(stmt) {
            return Err(CoreError::PolicyDenied(format!(
                "{}{kw} is not run on Redshift connections: the Redshift Data API runs and \
                 commits each statement on its own, so there are no multi-statement \
                 transactions and a BEGIN … COMMIT script would not be atomic. Run the \
                 statements without transaction control, or put them in a stored procedure",
                which(i, statements.len())
            )));
        }
    }
    Ok(())
}

/// `SHOW …` with nothing the narrow check cannot reason about. Every Redshift `SHOW` form
/// (`SHOW TABLES FROM SCHEMA db.s`, `SHOW COLUMNS FROM TABLE db.s.t`, `SHOW DATABASES`,
/// `SHOW VIEW v`, `SHOW TABLE t`, `SHOW GRANTS …`, `SHOW <parameter>`) only reads.
fn is_plain_show(stmt: &str) -> bool {
    let w = words(stmt);
    !w.has_comment
        && !w.has_semicolon
        && !w.has_dollar_quote
        && w.balanced
        && w.words.first().is_some_and(|f| f == "SHOW")
}

/// Refuses anything that is not provably a read. Fails closed.
pub(crate) fn check_read_only(statements: &[String]) -> Result<()> {
    for (i, stmt) in statements.iter().enumerate() {
        let c = classify(stmt);
        let ok = match c.overall {
            StatementClass::ReadOnly => c.parse_ok,
            // Redshift SHOW forms the PostgreSQL parser rejects. A parsed query that the
            // classifier flagged (a call to a side-effecting function) is not a SHOW and stays
            // denied.
            StatementClass::Unknown => is_plain_show(stmt),
            _ => false,
        };
        if !ok {
            return Err(CoreError::PolicyDenied(format!(
                "read-only run: {}{} is not a plain query. Redshift (through the Data API) has no \
                 read-only sessions, so only SELECT, WITH, SHOW and EXPLAIN of a query are \
                 submitted",
                which(i, statements.len()),
                c.summary
            )));
        }
    }
    Ok(())
}

/// A short command tag for `StatementDone`: `SELECT`, `INSERT`, `CREATE TABLE`, …
pub(crate) fn command_tag(stmt: &str) -> Option<String> {
    let w = words(stmt).words;
    let first = w.first()?.as_str();
    match first {
        "CREATE" | "DROP" | "ALTER" => {
            const NOISE: &[&str] = &[
                "OR",
                "REPLACE",
                "EXTERNAL",
                "TEMPORARY",
                "TEMP",
                "LOCAL",
                "MATERIALIZED",
                "IF",
                "NOT",
                "EXISTS",
            ];
            let object = w[1..].iter().find(|x| !NOISE.contains(&x.as_str()));
            Some(match object {
                Some(o) => format!("{first} {o}"),
                None => first.to_string(),
            })
        }
        "WITH" => Some("SELECT".to_string()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stmts(sql: &str) -> Vec<String> {
        crate::sql::split_statements(sql)
    }

    #[test]
    fn transaction_control_is_found_after_comments_and_in_any_case() {
        for (sql, kw) in [
            ("BEGIN", "BEGIN"),
            ("begin transaction", "BEGIN"),
            ("-- hi\nCOMMIT", "COMMIT"),
            ("/* a /* nested */ b */ rollback", "ROLLBACK"),
            ("START TRANSACTION READ ONLY", "START TRANSACTION"),
            ("END TRANSACTION", "END"),
            ("ABORT", "ABORT"),
            ("SAVEPOINT s1", "SAVEPOINT"),
            ("RELEASE SAVEPOINT s1", "RELEASE SAVEPOINT"),
        ] {
            assert_eq!(transaction_control(sql).as_deref(), Some(kw), "{sql}");
        }
        for sql in [
            "SELECT 'BEGIN'",
            "SELECT \"commit\" FROM t",
            "START",
            "CREATE PROCEDURE p() AS $$ BEGIN NULL; END; $$ LANGUAGE plpgsql",
            "SELECT 1",
            "",
        ] {
            assert_eq!(transaction_control(sql), None, "{sql}");
        }
    }

    #[test]
    fn a_script_with_transaction_control_is_refused_whole_with_the_reason() {
        let err =
            check_no_transaction_control(&stmts("BEGIN; UPDATE t SET a = 1; COMMIT")).unwrap_err();
        let m = err.to_string();
        assert!(m.contains("statement 1 of 3") && m.contains("BEGIN"), "{m}");
        assert!(m.contains("commits each statement on its own"), "{m}");
        check_no_transaction_control(&stmts("UPDATE t SET a = 1; SELECT 1")).unwrap();
    }

    #[test]
    fn read_only_admits_queries_and_show_forms() {
        for sql in [
            "SELECT 1",
            "WITH a AS (SELECT 1) SELECT * FROM a",
            "SELECT * FROM sales WHERE note = 'drop table x'",
            "EXPLAIN SELECT * FROM sales",
            "SHOW search_path",
            "SHOW TABLES FROM SCHEMA dev.public",
            "SHOW COLUMNS FROM TABLE dev.public.sales",
            "SHOW DATABASES",
            "SHOW VIEW public.v",
        ] {
            check_read_only(&stmts(sql)).unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
    }

    #[test]
    fn read_only_refuses_everything_else() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "CREATE TABLE t AS SELECT 1",
            "SELECT * INTO new_t FROM t",
            "DROP TABLE t",
            "TRUNCATE t",
            "UNLOAD ('select 1') TO 's3://b/p' IAM_ROLE default",
            "COPY t FROM 's3://b/p' IAM_ROLE default",
            "CALL p()",
            "VACUUM t",
            "ANALYZE t",
            "GRANT SELECT ON t TO u",
            "SET search_path TO x",
            "SELECT 1; DELETE FROM t",
            "SELECT 'unterminated",
            "SHOW TABLES; DROP TABLE t",
            "SHOW TABLES -- x",
            "this is not sql",
            "SELECT pg_terminate_backend(123)",
        ] {
            let err = check_read_only(&stmts(sql)).unwrap_err();
            assert!(
                matches!(&err, CoreError::PolicyDenied(m) if m.starts_with("read-only run")),
                "{sql}: {err:?}"
            );
        }
    }

    #[test]
    fn command_tags() {
        assert_eq!(command_tag("select 1").as_deref(), Some("SELECT"));
        assert_eq!(
            command_tag("WITH a AS (SELECT 1) SELECT 1").as_deref(),
            Some("SELECT")
        );
        assert_eq!(
            command_tag("create temp table t (a int)").as_deref(),
            Some("CREATE TABLE")
        );
        assert_eq!(
            command_tag("CREATE OR REPLACE VIEW v AS SELECT 1").as_deref(),
            Some("CREATE VIEW")
        );
        assert_eq!(
            command_tag("CREATE MATERIALIZED VIEW mv AS SELECT 1").as_deref(),
            Some("CREATE VIEW")
        );
        assert_eq!(command_tag("-- c\nshow tables").as_deref(), Some("SHOW"));
        assert_eq!(command_tag("").as_deref(), None);
    }
}
