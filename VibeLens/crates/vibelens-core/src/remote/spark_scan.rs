//! The read-only check for Spark SQL. It is the enforcement (Livy has no read-only session), so
//! it fails closed: a statement is admitted only when it is provably a read.
//!
//! A Spark `SELECT` can do more than a Trino one, so on top of "starts with SELECT, WITH, SHOW,
//! DESCRIBE or EXPLAIN and has no write keyword" it refuses:
//!
//! * `reflect`, `java_method` and `try_reflect`, which call arbitrary static Java methods in the
//!   driver (also when the name is quoted or qualified);
//! * direct file relations, `` parquet.`/path` ``, `` text.`file:///…` `` and any `` x.`…` ``
//!   whose quoted part has a `/` or `:`: they read any file the driver can, which on
//!   Kubernetes includes its service-account token;
//! * `TRANSFORM` and `MAP`/`REDUCE … USING 'script'` (they run a program), `IDENTIFIER(…)` and
//!   `EXECUTE IMMEDIATE` (they turn a string, which this scan does not look into, into SQL), and
//!   `LATERAL VIEW` over anything but Spark's built-in generators;
//! * everything that is not a read: `ADD JAR`, `SET`, `RESET`, `CACHE`, `REFRESH`, `LOAD`, DML,
//!   DDL.
//!
//! The scan tokenises with Spark's lexical rules (string literals in `'…'` and `"…"` with
//! backslash escapes, identifiers in backticks with doubled backticks, `--` and `/* */`
//! comments). Text it cannot tokenise (an unterminated literal, a raw string, a character that
//! is not part of Spark's syntax, a `;`) is refused.

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// A bare word, upper-cased.
    Word(String),
    /// A backtick-quoted identifier, unquoted (or a double-quoted string, which Spark reads as
    /// an identifier when `spark.sql.ansi.doubleQuotedIdentifiers` is on).
    Quoted(String),
    /// A single-quoted string literal (content dropped).
    Str,
    Punct(char),
}

/// Spark functions that call Java methods by name.
const JAVA_CALLS: &[&str] = &["REFLECT", "JAVA_METHOD", "TRY_REFLECT"];

/// Data source names Spark accepts as `format.`path`` relations.
const FILE_FORMATS: &[&str] = &[
    "PARQUET",
    "TEXT",
    "CSV",
    "JSON",
    "ORC",
    "AVRO",
    "BINARYFILE",
    "DELTA",
    "HIVE",
    "JDBC",
    "XML",
    "ICEBERG",
];

/// Words that start or smuggle in something that is not a read.
const DENY_WORDS: &[&str] = &[
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "CREATE",
    "DROP",
    "ALTER",
    "TRUNCATE",
    "CALL",
    "GRANT",
    "REVOKE",
    "MSCK",
    "REPAIR",
    "ANALYZE",
    "OPTIMIZE",
    "VACUUM",
    "ADD",
    "SET",
    "RESET",
    "CACHE",
    "UNCACHE",
    "REFRESH",
    "LOAD",
    "OVERWRITE",
    "INTO",
    "USE",
    "TRANSFORM",
    "REDUCE",
    "EXECUTE",
    "IMMEDIATE",
    "IDENTIFIER",
    "DECLARE",
    "EXPORT",
    "IMPORT",
];

/// Built-in generators allowed after `LATERAL VIEW [OUTER]`.
const BUILTIN_GENERATORS: &[&str] = &[
    "EXPLODE",
    "EXPLODE_OUTER",
    "POSEXPLODE",
    "POSEXPLODE_OUTER",
    "INLINE",
    "INLINE_OUTER",
    "STACK",
    "JSON_TUPLE",
    "PARSE_URL_TUPLE",
];

const FIRST_WORDS: &[&str] = &["SELECT", "WITH", "SHOW", "DESCRIBE", "DESC", "EXPLAIN"];

/// Tokenises `sql` with Spark's lexical rules. `Err` carries why it could not.
///
/// `escapes` says whether a backslash escapes the next character inside a literal. It does by
/// default, but not with `spark.sql.parser.escapedStringLiterals=true`, which the cluster can
/// set without the profile knowing; [`refusal`] therefore reads every statement both ways.
fn tokenize(sql: &str, escapes: bool) -> std::result::Result<Vec<Tok>, &'static str> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                loop {
                    if i + 1 >= chars.len() {
                        return Err("an unterminated comment");
                    }
                    if chars[i] == '*' && chars[i + 1] == '/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            '\'' | '"' => {
                // A raw string (r'…') has no escapes: reading it with them could hide text.
                if matches!(out.last(), Some(Tok::Word(w)) if w == "R")
                    && i > 0
                    && !chars[i - 1].is_whitespace()
                {
                    return Err("a raw string literal");
                }
                let quote = c;
                let mut content = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err("an unterminated string literal"),
                        Some('\\') if escapes => {
                            if let Some(&n) = chars.get(i + 1) {
                                content.push(n);
                            }
                            i += 2;
                        }
                        Some(&q) if q == quote => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            content.push(ch);
                            i += 1;
                        }
                    }
                }
                out.push(if quote == '"' {
                    Tok::Quoted(content)
                } else {
                    Tok::Str
                });
            }
            '`' => {
                let mut content = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err("an unterminated quoted identifier"),
                        Some('`') if chars.get(i + 1) == Some(&'`') => {
                            content.push('`');
                            i += 2;
                        }
                        Some('`') => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            content.push(ch);
                            i += 1;
                        }
                    }
                }
                out.push(Tok::Quoted(content));
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let w: String = chars[start..i].iter().collect();
                out.push(Tok::Word(w.to_ascii_uppercase()));
            }
            '(' | ')' | ',' | '.' | '*' | '+' | '-' | '/' | '%' | '=' | '<' | '>' | '!' | '|'
            | '&' | '^' | '~' | '[' | ']' | ':' | '?' | '{' | '}' => {
                out.push(Tok::Punct(c));
                i += 1;
            }
            ';' => return Err("a ';' inside the statement"),
            _ => return Err("a character that is not part of Spark SQL's syntax"),
        }
    }
    Ok(out)
}

/// Why `stmt` is not provably a read, or `None` when it is. The statement must pass when read
/// with backslash escapes and when read without them (see [`tokenize`]).
fn refusal(stmt: &str) -> Option<String> {
    refusal_as(stmt, true).or_else(|| refusal_as(stmt, false))
}

fn refusal_as(stmt: &str, escapes: bool) -> Option<String> {
    let toks = match tokenize(stmt, escapes) {
        Ok(t) => t,
        Err(why) => return Some(format!("it contains {why}, which the check cannot read")),
    };
    match toks.first() {
        Some(Tok::Word(w)) if FIRST_WORDS.contains(&w.as_str()) => {}
        Some(Tok::Word(w)) => return Some(format!("{w} is not a query")),
        _ => return Some("it does not start with SELECT, WITH, SHOW, DESCRIBE or EXPLAIN".into()),
    }
    for (i, t) in toks.iter().enumerate() {
        let next = toks.get(i + 1);
        let prev = i.checked_sub(1).and_then(|p| toks.get(p));
        match t {
            Tok::Word(w) if JAVA_CALLS.contains(&w.as_str()) => {
                return Some(format!(
                    "it calls {}, which runs a Java method in the Spark driver",
                    w.to_ascii_lowercase()
                ));
            }
            Tok::Quoted(q) if JAVA_CALLS.contains(&q.to_ascii_uppercase().as_str()) => {
                return Some(format!(
                    "it calls {}, which runs a Java method in the Spark driver",
                    q.to_ascii_lowercase()
                ));
            }
            Tok::Word(w) if DENY_WORDS.contains(&w.as_str()) => {
                return Some(format!(
                    "it contains {w}, which is not part of a plain query"
                ));
            }
            Tok::Word(w) if w == "USING" && matches!(next, Some(Tok::Str)) => {
                return Some("it runs a script (USING '…')".into());
            }
            Tok::Word(w) if w == "LATERAL" && matches!(next, Some(Tok::Word(v)) if v == "VIEW") => {
                let mut j = i + 2;
                if matches!(toks.get(j), Some(Tok::Word(o)) if o == "OUTER") {
                    j += 1;
                }
                let builtin = matches!(
                    (toks.get(j), toks.get(j + 1)),
                    (Some(Tok::Word(g)), Some(Tok::Punct('(')))
                        if BUILTIN_GENERATORS.contains(&g.as_str())
                );
                if !builtin {
                    return Some(
                        "it uses LATERAL VIEW over a function that is not a built-in generator"
                            .into(),
                    );
                }
            }
            // `format.`path`` (or `` `format`.`path` ``): a direct file relation.
            Tok::Quoted(q) if matches!(prev, Some(Tok::Punct('.'))) => {
                let source = match i.checked_sub(2).and_then(|p| toks.get(p)) {
                    Some(Tok::Word(w)) => Some(w.clone()),
                    Some(Tok::Quoted(w)) => Some(w.to_ascii_uppercase()),
                    _ => None,
                };
                let is_format = source.as_deref().is_some_and(|s| FILE_FORMATS.contains(&s));
                if is_format || q.contains('/') || q.contains(':') {
                    return Some(
                        "it reads a file path directly (format.`path`), which can reach any file \
                         the Spark driver can"
                            .into(),
                    );
                }
            }
            _ => {}
        }
    }
    None
}

/// Refuses every statement of `statements` that is not provably a read, before any is sent.
pub fn check_spark_read_only(statements: &[String]) -> Result<()> {
    for (i, stmt) in statements.iter().enumerate() {
        if let Some(why) = refusal(stmt) {
            return Err(CoreError::PolicyDenied(format!(
                "read-only run: {}not submitted because {why}. Spark has no read-only sessions, \
                 so only plain SELECT, WITH, SHOW, DESCRIBE and EXPLAIN statements are submitted",
                super::common::which(i, statements.len())
            )));
        }
    }
    Ok(())
}

/// Statements that change the shared Livy session for the statements after them. Refused in
/// every mode: each statement must run against the catalog and namespace the connection names
/// (and the approval card shows).
pub(crate) fn session_statement(stmt: &str) -> Option<&'static str> {
    let toks = tokenize(stmt, true).ok()?;
    match toks.first() {
        Some(Tok::Word(w)) => match w.as_str() {
            "USE" => Some("USE"),
            "SET" => Some("SET"),
            "RESET" => Some("RESET"),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> bool {
        refusal(s).is_none()
    }

    #[test]
    fn plain_reads_are_admitted() {
        for s in [
            "SELECT * FROM lake.shop.orders LIMIT 10",
            "select id, `weird name` from orders where note = 'drop table x; reflect(1)'",
            "WITH a AS (SELECT 1 AS x) SELECT x FROM a",
            "SHOW TABLES IN lake.shop",
            "DESCRIBE TABLE EXTENDED lake.shop.orders",
            "DESC orders",
            "EXPLAIN SELECT 1",
            "SELECT * FROM orders VERSION AS OF 123",
            "SELECT * FROM orders TIMESTAMP AS OF '2024-01-01 00:00:00'",
            "SELECT * FROM lake.shop.orders.snapshots",
            "SELECT c FROM t LATERAL VIEW explode(arr) x AS c",
            "SELECT c FROM t LATERAL VIEW OUTER posexplode(arr) x AS p, c",
            "SELECT 'its' AS s -- trailing comment",
            "SELECT /* hint */ 1",
            "SELECT a FROM t JOIN u USING (id)",
            "SELECT \"text\" AS s",
        ] {
            assert!(ok(s), "{s}: {:?}", refusal(s));
        }
    }

    /// Without these refusals a read-only run (every agent run without approval) could call a
    /// Java method in the driver or read its service-account token.
    #[test]
    fn java_calls_and_file_relations_are_refused() {
        for s in [
            "SELECT reflect('java.lang.Runtime', 'getRuntime')",
            "SELECT java_method('java.lang.System', 'exit', 0)",
            "SELECT try_reflect('java.lang.System', 'getenv', 'X')",
            "SELECT `reflect`('java.lang.System', 'getenv', 'X')",
            "SELECT system.builtin.reflect('a', 'b')",
            "SELECT REFLECT ('a','b')",
            "SELECT * FROM text.`file:///var/run/secrets/kubernetes.io/serviceaccount/token`",
            "SELECT * FROM text.`/var/run/secrets/kubernetes.io/serviceaccount/token`",
            "SELECT * FROM parquet.`s3a://bucket/path/`",
            "SELECT * FROM csv.`relative-file`",
            "SELECT * FROM binaryFile.`/etc`",
            "SELECT * FROM delta.`/tmp/x`",
            "SELECT * FROM `text`.`/etc/passwd`",
            "SELECT * FROM weird.`/etc/passwd`",
            "SELECT * FROM t WHERE id IN (SELECT value FROM text.`/etc/hosts`)",
            "WITH x AS (SELECT * FROM json.`hdfs:///p`) SELECT * FROM x",
        ] {
            assert!(!ok(s), "{s}");
        }
    }

    #[test]
    fn writes_session_changes_and_scripts_are_refused() {
        for s in [
            "INSERT INTO t VALUES (1)",
            "INSERT OVERWRITE t SELECT 1",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "CREATE TABLE t AS SELECT 1",
            "DROP TABLE t",
            "ALTER TABLE t ADD COLUMNS (x int)",
            "ADD JAR s3://b/evil.jar",
            "ADD FILE /tmp/x",
            "SET spark.sql.x = 1",
            "RESET",
            "CACHE TABLE t",
            "CACHE LAZY TABLE c AS SELECT 1",
            "REFRESH TABLE t",
            "LOAD DATA INPATH '/x' INTO TABLE t",
            "USE lake.shop",
            "CALL lake.system.rollback_to_snapshot('shop.orders', 1)",
            "ANALYZE TABLE t COMPUTE STATISTICS",
            "MSCK REPAIR TABLE t",
            "SELECT TRANSFORM(a) USING 'cat' AS (b) FROM t",
            "FROM t MAP a USING '/bin/sh -c id' AS b",
            "FROM t SELECT a",
            "SELECT * FROM IDENTIFIER('text.`/etc/passwd`')",
            "EXECUTE IMMEDIATE 'DROP TABLE t'",
            "SELECT c FROM t LATERAL VIEW my_udtf(a) x AS c",
            "SELECT 1; DROP TABLE t",
            "SELECT r'\\', reflect('a','b'), 'x'",
            "SELECT 'unterminated",
            "SELECT `unterminated",
            "SELECT 1 /* open",
            "SELECT $1",
            "SELECT 1 \\ 2",
            "(SELECT 1)",
            "",
            "VALUES (1)",
            "WITH a AS (SELECT 1) INSERT INTO t SELECT * FROM a",
            "EXPLAIN INSERT INTO t SELECT 1",
        ] {
            assert!(!ok(s), "{s}");
        }
    }

    #[test]
    fn string_and_identifier_contents_do_not_count_as_code() {
        // The scan reads literals out, so words inside them are not seen as keywords...
        assert!(ok("SELECT 'insert into reflect(x)' AS s"));
        assert!(ok("SELECT a FROM t WHERE a RLIKE '\\\\d+'"));
        // ...and a statement must pass read both with and without backslash escapes: with
        // `spark.sql.parser.escapedStringLiterals=true` on the cluster, `'a\'` ends at the
        // second quote and the reflect call below is code.
        assert!(!ok("SELECT 'a\\' , reflect('x','y') , '"));
        // An odd backslash before a quote reads differently the two ways: refused.
        assert!(!ok("SELECT 'it\\'s' AS s"));
    }

    #[test]
    fn the_script_check_names_the_statement() {
        let e = check_spark_read_only(&["SELECT 1".into(), "DROP TABLE t".into()]).unwrap_err();
        let m = e.to_string();
        assert!(m.contains("statement 2 of 2") && m.contains("DROP"), "{m}");
        assert!(matches!(e, CoreError::PolicyDenied(_)));
        check_spark_read_only(&["SELECT 1".into(), "SHOW TABLES".into()]).unwrap();
    }

    #[test]
    fn session_changes_are_recognised() {
        assert_eq!(session_statement("use lake.shop"), Some("USE"));
        assert_eq!(session_statement("SET a=b"), Some("SET"));
        assert_eq!(session_statement("RESET"), Some("RESET"));
        assert_eq!(session_statement("SELECT 1"), None);
    }
}
