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
    "LIBSVM",
    "IMAGE",
    "KAFKA",
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
    "RECORDWRITER",
    "RECORDREADER",
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
fn tokenize(sql: &str, escapes: bool) -> std::result::Result<Lexed, &'static str> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut has_comment = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '-' if chars.get(i + 1) == Some(&'-') => {
                has_comment = true;
                while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' {
                    // Spark continues a `--` comment over a backslash-newline; where it ends
                    // is not something to guess.
                    if chars[i] == '\\' {
                        return Err("a backslash in a -- comment");
                    }
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                has_comment = true;
                // `/*+ … */` is a hint, which Spark parses.
                if chars.get(i + 2) == Some(&'+') {
                    return Err("a query hint");
                }
                i += 2;
                loop {
                    if i + 1 >= chars.len() {
                        return Err("an unterminated comment");
                    }
                    // Spark nests block comments; this scan does not.
                    if chars[i] == '/' && chars[i + 1] == '*' {
                        return Err("a nested block comment");
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
    Ok(Lexed {
        toks: out,
        has_comment,
    })
}

/// What [`tokenize`] found.
struct Lexed {
    toks: Vec<Tok>,
    has_comment: bool,
}

/// Why `stmt` must not be sent to Spark in any mode, or `None`.
///
/// Spark substitutes `${…}` (`spark.sql.variable.substitute`, on by default) in the raw text
/// before it parses anything, inside literals and comments too: `'${env:AWS_SECRET_ACCESS_KEY}'`
/// reads an environment variable of the driver, and a substitution inside a comment can turn
/// into code. No check on the text can know what it becomes, so it is refused everywhere.
pub(crate) fn substitution_refusal(stmt: &str) -> Option<String> {
    stmt.contains("${").then(|| {
        "it contains '${', which Spark replaces before parsing (variable substitution), so \
         what would run cannot be checked. Write the value out instead; a cluster admin can \
         also set spark.sql.variable.substitute=false"
            .to_string()
    })
}

/// Why `stmt` is not provably a read, or `None` when it is. The statement must pass when read
/// with backslash escapes and when read without them (see [`tokenize`]).
fn refusal(stmt: &str) -> Option<String> {
    refusal_as(stmt, true).or_else(|| refusal_as(stmt, false))
}

fn refusal_as(stmt: &str, escapes: bool) -> Option<String> {
    if let Some(why) = substitution_refusal(stmt) {
        return Some(why);
    }
    let toks = match tokenize(stmt, escapes) {
        // Comments are where Spark's lexer and this one can disagree (nesting, a backslash
        // before a newline, hints), so a read-only run has none, as on Athena and Trino.
        Ok(l) if l.has_comment => {
            return Some("it contains a comment, which read-only runs do not accept".into());
        }
        Ok(l) => l.toks,
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
            Tok::Word(w) if w == "USING" && matches!(next, Some(Tok::Str | Tok::Quoted(_))) => {
                return Some("it runs a script (USING '…')".into());
            }
            // `MAP …`/`REDUCE … USING 'script'`; `map(…)` and `reduce(…)` are functions.
            Tok::Word(w)
                if (w == "MAP" || w == "REDUCE") && !matches!(next, Some(Tok::Punct('('))) =>
            {
                return Some(format!("it contains {w}, which can run a script"));
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
                // A quoted source with a `.` in it is a data source class name.
                let is_format = source
                    .as_deref()
                    .is_some_and(|s| FILE_FORMATS.contains(&s) || s.contains('.'));
                if is_format || q.contains('/') || q.contains(':') {
                    return Some(
                        "it reads a file path directly (format.`path`), which can reach any file \
                         the Spark driver can"
                            .into(),
                    );
                }
            }
            // `text.passwd`, `` `csv`.x ``: with `spark.sql.runSQLOnFiles` a two-part name whose
            // table does not exist is read as a file, so a two-part name that starts with a
            // file format is refused, quoted or not.
            Tok::Word(_) | Tok::Quoted(_)
                if matches!(next, Some(Tok::Punct('.')))
                    && !matches!(prev, Some(Tok::Punct('.')))
                    && !matches!(toks.get(i + 3), Some(Tok::Punct('.')))
                    && is_format_name(t) =>
            {
                let name = match t {
                    Tok::Word(w) | Tok::Quoted(w) => w.to_ascii_lowercase(),
                    _ => String::new(),
                };
                return Some(format!(
                    "it names a two-part relation starting with '{name}', which Spark can read as a \
                     file. A namespace called '{name}' must be written with its catalog in front \
                     (catalog.{name}.table)"
                ));
            }
            _ => {}
        }
    }
    None
}

fn is_format_name(t: &Tok) -> bool {
    match t {
        Tok::Word(w) => FILE_FORMATS.contains(&w.as_str()),
        Tok::Quoted(q) => FILE_FORMATS.contains(&q.to_ascii_uppercase().as_str()),
        _ => false,
    }
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
///
/// Fails closed: when the statement cannot be tokenised (a backslash in a `--` comment, a nested
/// block comment, a hint, …) it is refused if it starts with a comment, and otherwise judged by
/// its first word.
///
/// Session-scoped objects are refused for the same reason: one Livy session serves approved
/// writes and unapproved read-only runs, and the read-only scan knows names, not what a name
/// was bound to. A temporary function bound to `GenericUDFReflect`, or a temporary view that
/// shadows a table, created by an approved run would be used by every later unapproved read.
pub(crate) fn session_statement(stmt: &str) -> Option<&'static str> {
    let words: Vec<String> = match tokenize(stmt, true) {
        Ok(l) => l
            .toks
            .iter()
            .take(6)
            .map_while(|t| match t {
                Tok::Word(w) => Some(w.clone()),
                _ => None,
            })
            .collect(),
        Err(_) => {
            let t = stmt.trim_start();
            if t.starts_with("--") || t.starts_with("/*") {
                return Some("a statement that starts with a comment the check cannot read");
            }
            t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
                .take(6)
                .map(str::to_ascii_uppercase)
                .collect()
        }
    };
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    let temporary = |rest: &[&str]| {
        let rest: Vec<&str> = rest
            .iter()
            .copied()
            .filter(|w| !matches!(*w, "OR" | "REPLACE" | "GLOBAL"))
            .collect();
        matches!(rest.first(), Some(&("TEMPORARY" | "TEMP")))
    };
    Some(match w.as_slice() {
        ["USE", ..] => "USE",
        ["SET", ..] => "SET",
        ["RESET", ..] => "RESET",
        ["CREATE", rest @ ..] if temporary(rest) => "CREATE TEMPORARY …",
        ["DROP", rest @ ..] if temporary(rest) => "DROP TEMPORARY …",
        ["ADD", ..] => "ADD",
        ["DECLARE", ..] => "DECLARE",
        ["EXECUTE", ..] => "EXECUTE IMMEDIATE",
        ["CACHE", ..] => "CACHE",
        ["UNCACHE", ..] => "UNCACHE",
        ["CLEAR", ..] => "CLEAR CACHE",
        _ => return None,
    })
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
            "SELECT 'its' AS s",
            "SELECT map('a', 1), reduce(array(1, 2), 0, (a, x) -> a + x)",
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

    /// Audit 2026-11-21, items 1 to 3: places where Spark's lexer and parser read the text
    /// differently from this scan. Without these refusals each payload passes as a plain read
    /// and Spark runs something else.
    #[test]
    fn lexer_differences_with_spark_are_refused() {
        for s in [
            // `--` comments continue over a backslash-newline in Spark: the INSERT runs.
            "-- x \\\nSELECT '\nINSERT INTO lake.shop.orders SELECT * FROM lake.shop.orders -- '",
            // Nested block comments: Spark sees reflect(...) as code.
            "SELECT 1 /* /* */ ' */ + reflect(\"a\",\"b\") + /* ' */ 2",
            // Hints and plain comments: refused in read-only runs.
            "SELECT /*+ REPARTITION(3) */ 1",
            "SELECT /* c */ 1",
            "SELECT 1 -- c",
            // Script transform with a double-quoted script, and MAP/REDUCE without a paren.
            "SELECT * FROM (MAP a USING \"cat /var/run/secrets/kubernetes.io/serviceaccount/token\" FROM (SELECT 1 a) t) x",
            "SELECT * FROM (REDUCE a USING 'cat' AS b FROM t) x",
            "SELECT * FROM t ROW FORMAT SERDE 'x' RECORDWRITER 'y'",
            "SELECT * FROM t RECORDREADER 'y'",
            // More file sources, and a data source class name.
            "SELECT * FROM libsvm.`data`",
            "SELECT * FROM image.`imgs`",
            "SELECT * FROM kafka.`topic`",
            "SELECT * FROM `org.apache.spark.sql.execution.datasources.text.TextFileFormat`.`etc`",
        ] {
            assert!(!ok(s), "{s}");
        }
    }

    /// Audit item 2: `${…}` is substituted by Spark before parsing, inside literals and
    /// comments. Without this the first payload returns cluster secrets to the model.
    #[test]
    fn variable_substitution_is_refused() {
        for s in [
            "SELECT '${env:AWS_SECRET_ACCESS_KEY}', '${spark.hadoop.fs.s3a.secret.key}'",
            "SELECT 1 -- ${system:line.separator} , reflect('a', 'b')",
        ] {
            assert!(!ok(s), "{s}");
            assert!(substitution_refusal(s).is_some(), "{s}");
        }
        assert!(substitution_refusal("SELECT '$' || '{x}'").is_none());
    }

    /// Audit items 1, 3 and 10: the session check fails closed when it cannot tokenise.
    #[test]
    fn the_session_check_fails_closed() {
        assert!(session_statement("-- x \\\nUSE lake.other").is_some());
        assert!(session_statement("/* /* */ */ SET a=b").is_some());
        assert!(session_statement("/*+ x */ SET a=b").is_some());
        assert_eq!(session_statement("SET $x = 1"), Some("SET"));
        assert_eq!(session_statement("SELECT $x"), None);
        assert_eq!(session_statement("INSERT INTO t SELECT 1 -- note"), None);
    }

    /// Re-audit item 1: state an approved run leaves in the shared Livy session would be used
    /// by later unapproved reads, which the read-only scan checks by name only.
    #[test]
    fn session_scoped_state_is_refused() {
        for s in [
            "CREATE TEMPORARY FUNCTION f AS 'org.apache.hadoop.hive.ql.udf.generic.GenericUDFReflect'",
            "CREATE OR REPLACE TEMPORARY VIEW orders AS SELECT 1",
            "create or replace global temporary view v as select 1",
            "CREATE TEMP VIEW v AS SELECT 1",
            "CREATE TEMPORARY TABLE t USING parquet",
            "CREATE TEMPORARY VARIABLE x INT",
            "DROP TEMPORARY FUNCTION f",
            "DROP TEMPORARY VARIABLE x",
            "ADD JAR s3://b/x.jar",
            "DECLARE x INT DEFAULT 1",
            "SET VAR x = 2",
            "SET VARIABLE x = 2",
            "EXECUTE IMMEDIATE 'USE other.ns'",
            "EXECUTE IMMEDIATE 'SET spark.sql.ansi.enabled=false'",
            "CACHE TABLE t",
            "UNCACHE TABLE t",
            "CLEAR CACHE",
        ] {
            assert!(session_statement(s).is_some(), "{s}");
        }
        for s in [
            "CREATE TABLE lake.shop.t (a INT)",
            "CREATE OR REPLACE VIEW lake.shop.v AS SELECT 1",
            "DROP TABLE lake.shop.t",
            "INSERT INTO t SELECT 1",
        ] {
            assert_eq!(session_statement(s), None, "{s}");
        }
    }

    /// Re-audit item 3: with `spark.sql.runSQLOnFiles` a two-part name whose table is not
    /// found resolves as a file.
    #[test]
    fn unquoted_file_relations_are_refused() {
        for s in [
            "SELECT * FROM text.passwd",
            "SELECT * FROM csv.__spark_conf__",
            "SELECT * FROM `text`.somefile",
            "SELECT * FROM t JOIN json.x ON true",
        ] {
            let why = refusal(s).unwrap_or_default();
            assert!(
                why.contains("catalog.text")
                    || why.contains("catalog.csv")
                    || why.contains("catalog.json"),
                "{s}: {why}"
            );
        }
        for s in [
            "SELECT * FROM lake.text.passwd",
            "SELECT * FROM iceberg.shop.orders",
            "SELECT * FROM shop.orders",
        ] {
            assert!(ok(s), "{s}: {:?}", refusal(s));
        }
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
