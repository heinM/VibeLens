//! What may be submitted to Redshift, decided before any request is made.
//!
//! The Data API runs every statement in its own session and commits it on its own, and it has
//! no way to make a session read-only. So these checks run over the whole script first:
//!
//! * **One meaning only** ([`check_script`], every mode). Redshift reads `\'` inside `'…'` as an
//!   escaped quote; the PostgreSQL-dialect splitter and classifier read `'\'` as a whole string.
//!   A statement where the two readings differ is refused, and so is a statement that still holds
//!   a top-level `;` after splitting, so what is classified is what Redshift runs, one statement
//!   per request.
//! * **Transaction control is refused in every mode.** `BEGIN … COMMIT` would look atomic and
//!   not be: each statement has already committed by the time the next one starts.
//! * **A read-only run submits only statements that are provably reads (R5).** The
//!   PostgreSQL-dialect classifier must parse a statement and class it read-only; Redshift's
//!   `SHOW …` forms that the parser does not know are admitted by a narrow text check. On top of
//!   that every function call must be a Redshift built-in known to only read
//!   ([`unknown_function`]): nothing else (no READ ONLY transaction) would stop a Lambda UDF or a
//!   SageMaker model function from sending row data out. Anything else is refused.

use super::lex::{Lexed, Tok, lex};
use crate::error::{CoreError, Result};
use crate::sql::{SpanKind, StatementClass, classify, quoted_and_comment_spans};

/// Whether byte `p` of `stmt` lies inside a single-quoted literal (any prefix) or a double-quoted
/// identifier in `spans`. A dollar-quoted body does not count.
fn inside_quotes(stmt: &str, spans: &[(SpanKind, usize, usize)], p: usize) -> bool {
    spans.iter().any(|&(kind, start, end)| {
        start <= p
            && p < end
            && matches!(kind, SpanKind::Str | SpanKind::QuotedIdent)
            && stmt.as_bytes().get(start) != Some(&b'$')
    })
}

fn words(stmt: &str) -> Vec<String> {
    lex(stmt, true).words_upper()
}

/// The transaction-control keyword `stmt` starts with, if any: `BEGIN`, `START TRANSACTION`,
/// `COMMIT`, `END`, `ROLLBACK`, `ABORT`, and the savepoint statements that only make sense
/// inside a transaction.
fn transaction_control(stmt: &str) -> Option<String> {
    let w = words(stmt);
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

/// Refuses a statement that Redshift and the PostgreSQL-dialect splitter and classifier could
/// read differently: a backslash inside a `'…'` literal under either reading, or a `;` left at the
/// top level after splitting.
pub(crate) fn check_unambiguous(statements: &[String]) -> Result<()> {
    for (i, stmt) in statements.iter().enumerate() {
        if lex(stmt, false).backslash_in_string() || lex(stmt, true).backslash_in_string() {
            return Err(CoreError::PolicyDenied(format!(
                "{}a string literal contains a backslash. Redshift reads \\' inside a string \
                 as an escaped quote while standard SQL reads '\\' as a complete string, so \
                 VibeLens cannot tell what this statement would do and does not run it. Write a \
                 quote inside a string by doubling it ('it''s'), and a backslash as CHR(92)",
                which(i, statements.len())
            )));
        }
        // A non-ASCII byte outside a `'…'` literal or a `"…"` identifier (in a word, a comment, a
        // dollar-quote tag or body) is where lexers have been seen to disagree: `$é$` is a
        // dollar-quote tag to one and not to another, and every boundary after it moves. Refused
        // unless both this module's lexers and the classifier's place it inside quotes.
        let sql_spans = quoted_and_comment_spans(stmt);
        let rs_spans = lex(stmt, true).spans;
        let std_spans = lex(stmt, false).spans;
        let stray = stmt.bytes().enumerate().any(|(p, b)| {
            b >= 0x80
                && !(inside_quotes(stmt, &sql_spans, p)
                    && inside_quotes(stmt, &rs_spans, p)
                    && inside_quotes(stmt, &std_spans, p))
        });
        if stray {
            return Err(CoreError::PolicyDenied(format!(
                "{}the statement has a non-ASCII character outside a 'string' or a \"quoted \
                 identifier\" (in a name, a comment or a $tag$ quote). Those are read \
                 differently by different SQL lexers, so VibeLens cannot be sure what Redshift \
                 would run. Put the text in a 'string' or the name in \"double quotes\", and \
                 use ASCII $$ tags",
                which(i, statements.len())
            )));
        }
        // Belt and braces: the classifier and Redshift must agree on every string, quoted
        // identifier and comment, or nothing they say about the statement can be trusted.
        if sql_spans != rs_spans {
            return Err(CoreError::PolicyDenied(format!(
                "{}VibeLens's SQL reader and Redshift's would split this statement into strings \
                 and comments differently, so it is not run. Simplify the quoting",
                which(i, statements.len())
            )));
        }
        if lex(stmt, true).toks.contains(&Tok::Punct(';')) {
            return Err(CoreError::PolicyDenied(format!(
                "{}the statement still contains a ';' after the script was split, so Redshift \
                 could run more than one statement in one request. Separate statements at the \
                 top level only",
                which(i, statements.len())
            )));
        }
    }
    Ok(())
}

/// Every check that applies in every mode: one meaning only, no transaction control.
pub(crate) fn check_script(statements: &[String]) -> Result<()> {
    check_unambiguous(statements)?;
    check_no_transaction_control(statements)
}

/// `SHOW …` with nothing the narrow check cannot reason about. Every Redshift `SHOW` form
/// (`SHOW TABLES FROM SCHEMA db.s`, `SHOW COLUMNS FROM TABLE db.s.t`, `SHOW DATABASES`,
/// `SHOW VIEW v`, `SHOW TABLE t`, `SHOW GRANTS …`, `SHOW <parameter>`) only reads.
fn is_plain_show(stmt: &str) -> bool {
    let l = lex(stmt, true);
    !l.has_comment
        && !l.unbalanced
        && !l.toks.contains(&Tok::Punct(';'))
        && !l.toks.contains(&Tok::Punct('$'))
        && matches!(l.toks.first(), Some(Tok::Word(w)) if w == "show")
}

/// Words that are followed by `(` without being a function call: keywords and type names with
/// a length or precision.
const PAREN_KEYWORDS: &[&str] = &[
    "all",
    "and",
    "any",
    "array",
    "as",
    "between",
    "by",
    "case",
    "distinct",
    "else",
    "except",
    "exists",
    "filter",
    "from",
    "group",
    "having",
    "ilike",
    "in",
    "intersect",
    "is",
    "join",
    "lateral",
    "like",
    "limit",
    "minus",
    "not",
    "offset",
    "on",
    "or",
    "order",
    "over",
    "partition",
    "pivot",
    "qualify",
    "select",
    "similar",
    "some",
    "then",
    "top",
    "union",
    "unpivot",
    "using",
    "values",
    "when",
    "where",
    "with",
    "within",
    // Types: `CAST(x AS varchar(20))`, `x::numeric(12, 2)`.
    "binary",
    "bpchar",
    "char",
    "character",
    "decimal",
    "float",
    "interval",
    "nchar",
    "numeric",
    "nvarchar",
    "time",
    "timestamp",
    "timestamptz",
    "timetz",
    "varbinary",
    "varbyte",
    "varchar",
];

/// Redshift built-in SQL functions that only read, by category of the Redshift SQL reference:
/// aggregate, window, conditional, date and time, math, string, JSON, SUPER, VARBYTE, type
/// conversion, hash, and the system information functions that only read. Anything that
/// changes state (`change_*_priority`, `pg_cancel_backend`, `pg_terminate_backend`,
/// `set_config`, …), spatial functions and every user-defined, Python, Lambda or SageMaker
/// function are left out on purpose.
const READ_ONLY_FUNCTIONS: &[&str] = &[
    // Aggregate.
    "any_value",
    "approximate",
    "avg",
    "bit_and",
    "bit_or",
    "bool_and",
    "bool_or",
    "count",
    "listagg",
    "max",
    "median",
    "min",
    "percentile_cont",
    "percentile_disc",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "sum",
    "var_pop",
    "var_samp",
    "variance",
    // Window.
    "cume_dist",
    "dense_rank",
    "first_value",
    "lag",
    "last_value",
    "lead",
    "nth_value",
    "ntile",
    "percent_rank",
    "rank",
    "ratio_to_report",
    "row_number",
    // Conditional.
    "coalesce",
    "decode",
    "greatest",
    "least",
    "nullif",
    "nvl",
    "nvl2",
    // Date and time.
    "add_months",
    "convert_timezone",
    "current_date",
    "date",
    "date_add",
    "date_cmp",
    "date_cmp_timestamp",
    "date_cmp_timestamptz",
    "date_part",
    "date_part_year",
    "date_trunc",
    "dateadd",
    "datediff",
    "datepart",
    "extract",
    "getdate",
    "interval_cmp",
    "last_day",
    "months_between",
    "next_day",
    "timeofday",
    "timestamp_cmp",
    "timestamp_cmp_date",
    "timestamp_cmp_timestamptz",
    "timestamptz_cmp",
    "timestamptz_cmp_date",
    "timestamptz_cmp_timestamp",
    "timezone",
    "to_timestamp",
    "trunc",
    // Math.
    "abs",
    "acos",
    "asin",
    "atan",
    "atan2",
    "cbrt",
    "ceil",
    "ceiling",
    "cos",
    "cot",
    "degrees",
    "dexp",
    "dlog1",
    "dlog10",
    "exp",
    "floor",
    "ln",
    "log",
    "mod",
    "pi",
    "pow",
    "power",
    "radians",
    "random",
    "round",
    "sign",
    "sin",
    "sqrt",
    "tan",
    // String.
    "ascii",
    "bpcharcmp",
    "btrim",
    "bttext_pattern_cmp",
    "char_length",
    "character_length",
    "charindex",
    "chr",
    "collate",
    "concat",
    "difference",
    "initcap",
    "left",
    "len",
    "length",
    "lower",
    "lpad",
    "ltrim",
    "octet_length",
    "oct_length",
    "position",
    "quote_ident",
    "quote_literal",
    "regexp_count",
    "regexp_instr",
    "regexp_replace",
    "regexp_substr",
    "repeat",
    "replace",
    "replicate",
    "reverse",
    "right",
    "rpad",
    "rtrim",
    "soundex",
    "split_part",
    "strpos",
    "strtol",
    "substr",
    "substring",
    "textlen",
    "translate",
    "trim",
    "upper",
    // JSON.
    "can_json_parse",
    "is_valid_json",
    "is_valid_json_array",
    "json_array_length",
    "json_extract_array_element_text",
    "json_extract_path_text",
    "json_parse",
    "json_serialize",
    "json_serialize_to_varbyte",
    "json_typeof",
    // SUPER.
    "array_concat",
    "array_flatten",
    "get_array_length",
    "is_array",
    "is_bigint",
    "is_boolean",
    "is_char",
    "is_decimal",
    "is_float",
    "is_integer",
    "is_object",
    "is_scalar",
    "is_smallint",
    "is_varchar",
    "object",
    "object_transform",
    "split_to_array",
    "subarray",
    // VARBYTE.
    "from_hex",
    "from_varbyte",
    "getbit",
    "to_varbyte",
    // Type conversion and formatting.
    "cast",
    "convert",
    "text_to_int_alt",
    "text_to_numeric_alt",
    "to_char",
    "to_date",
    "to_hex",
    "to_number",
    // Hash.
    "checksum",
    "crc32",
    "farmfingerprint64",
    "fnv_hash",
    "func_sha1",
    "md5",
    "sha",
    "sha1",
    "sha2",
    // System information that only reads.
    "current_database",
    "current_schema",
    "current_schemas",
    "current_setting",
    "current_user_id",
    "has_assumerole_privilege",
    "has_column_privilege",
    "has_database_privilege",
    "has_function_privilege",
    "has_language_privilege",
    "has_schema_privilege",
    "has_table_privilege",
    "pg_backend_pid",
    "pg_get_cols",
    "pg_get_late_binding_view_cols",
    "pg_last_copy_count",
    "pg_last_copy_id",
    "pg_last_query_id",
    "pg_last_unload_count",
    "pg_last_unload_id",
    "slice_num",
    "version",
];

/// The first call in `stmt` (Redshift reading) to a function that is not a Redshift built-in
/// known to only read, as `name()` or `schema.name()`. A call qualified with any schema but
/// `pg_catalog` counts, whatever its name: built-ins live in `pg_catalog`, so anything else is a
/// user-defined (possibly Lambda or SageMaker) function. A quoted name must match a built-in
/// exactly, because with case-sensitive identifiers `"LOWER"` is not `lower`.
fn first_unknown_call(l: &Lexed) -> Option<String> {
    let t = &l.toks;
    for k in 0..t.len() {
        if t.get(k + 1) != Some(&Tok::Punct('(')) {
            continue;
        }
        let name = match &t[k] {
            Tok::Word(w) => w.clone(),
            Tok::QuotedIdent(q) => q.clone(),
            _ => continue,
        };
        let schema = if k >= 2 && t[k - 1] == Tok::Punct('.') {
            match &t[k - 2] {
                Tok::Word(s) => Some(s.clone()),
                Tok::QuotedIdent(s) => Some(s.clone()),
                _ => Some(String::new()),
            }
        } else {
            None
        };
        match schema.as_deref() {
            Some("pg_catalog") | None => {}
            Some(s) => return Some(format!("{s}.{name}()")),
        }
        let keyword = schema.is_none()
            && matches!(t[k], Tok::Word(_))
            && PAREN_KEYWORDS.contains(&name.as_str());
        if !keyword && !READ_ONLY_FUNCTIONS.contains(&name.as_str()) {
            return Some(format!("{name}()"));
        }
    }
    None
}

/// The first function call in `statements` that a Redshift read-only run refuses (see
/// [`first_unknown_call`]), with its statement prefix.
pub(crate) fn unknown_function(statements: &[String]) -> Option<String> {
    statements.iter().enumerate().find_map(|(i, s)| {
        first_unknown_call(&lex(s, true)).map(|f| format!("{}{f}", which(i, statements.len())))
    })
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
    if let Some(f) = unknown_function(statements) {
        return Err(CoreError::PolicyDenied(format!(
            "read-only run: {f} is not a Redshift built-in function known to only read. A \
             user-defined, Lambda or SageMaker function can change data or send row values out \
             of the warehouse, and Redshift has no read-only session to stop it, so the \
             statement is not run read-only. It can be run with approval instead"
        )));
    }
    Ok(())
}

/// A short command tag for `StatementDone`: `SELECT`, `INSERT`, `CREATE TABLE`, …
pub(crate) fn command_tag(stmt: &str) -> Option<String> {
    let w = words(stmt);
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

    /// The two payloads of the 2026-11 audit (HIGH 1). The classifier reads the first as a
    /// SELECT of two literals; Redshift runs `SELECT … INTO public.pwned FROM secrets`. The
    /// second hides `pg_terminate_backend` the same way.
    const BACKSLASH_PAYLOADS: [&str; 2] = [
        r"SELECT '\' , ' AS x INTO public.pwned FROM secrets -- '",
        r"SELECT '\' , ' , pg_terminate_backend(123) -- '",
    ];

    #[test]
    fn backslash_payloads_are_refused_in_every_mode() {
        for sql in BACKSLASH_PAYLOADS {
            // Without the check the classifier calls it a plain SELECT.
            assert_eq!(classify(sql).overall, StatementClass::ReadOnly, "{sql}");
            let m = check_script(&stmts(sql)).unwrap_err().to_string();
            assert!(m.contains("backslash") && m.contains("CHR(92)"), "{m}");
        }
        for sql in [
            r"SELECT 'a\b'",
            r"SELECT 'x' || '\'",
            r"SELECT E'\''",
            r"UPDATE t SET a = 'C:\temp'",
        ] {
            assert!(check_script(&stmts(sql)).is_err(), "{sql}");
        }
        // Backslashes outside string literals, and doubled quotes, are fine.
        for sql in [
            "SELECT 'it''s', chr(92)",
            r#"SELECT "a\b" FROM t"#,
            "SELECT 1 -- a \\ comment",
            "CREATE FUNCTION f() RETURNS int AS $$ select 1 $$ LANGUAGE sql",
        ] {
            check_script(&stmts(sql)).unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
    }

    /// Re-audit: a non-ASCII dollar-quote tag. `crate::sql` reads `$é$ … $é$` as a string; a lexer
    /// with an ASCII-only tag rule does not, and every boundary after it moves.
    #[test]
    fn non_ascii_outside_quotes_is_refused() {
        for sql in [
            r"SELECT $é$ ' $é$ , '\' , ' AS x INTO public.pwned FROM secrets -- '",
            "SELECT $é$ ' $é$ , exfil_lambda(ssn) , $é$ ' $é$ FROM customers",
            "SELECT $é$ x $é$",
            "SELECT café FROM t",
            "SELECT 1 -- naïve comment",
            "SELECT $$ é $$",
        ] {
            assert!(check_script(&stmts(sql)).is_err(), "{sql}");
        }
        for sql in [
            "SELECT 'café', \"naïve\" FROM t",
            "SELECT E'é', U&'é', N'é' FROM t",
        ] {
            check_script(&stmts(sql)).unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
    }

    /// Property: whatever `check_unambiguous` admits, the classifier's lexer and Redshift's
    /// agree on every string, quoted identifier and comment span. The corpus holds every payload
    /// of both audits and the quoting forms that differ between dialects.
    #[test]
    fn admitted_statements_have_identical_spans_under_both_lexers() {
        let corpus = [
            r"SELECT '\' , ' AS x INTO public.pwned FROM secrets -- '",
            r"SELECT '\' , ' , pg_terminate_backend(123) -- '",
            r"SELECT $é$ ' $é$ , '\' , ' AS x INTO public.pwned FROM secrets -- '",
            "SELECT $é$ ' $é$ , exfil_lambda(ssn) , $é$ ' $é$ FROM customers",
            "SELECT f_notify(card_number) FROM payments",
            r"SELECT E'\'' , 1",
            "SELECT E'abc', B'101', X'ff', N'x'",
            "SELECT U&'d\\0061t' , U&\"d\\0061t\" FROM t",
            "SELECT $$ it's; $$, $tag$ a $$ b $tag$, $1",
            "SELECT $a$ x $b$ y $a$ , 2",
            "SELECT /* a /* nested ' */ still */ 1",
            "SELECT 1 -- comment ' \r, 'after CR'",
            "SELECT 1 -- comment \n, 'after LF'",
            "SELECT 'a''b', \"c\"\"d\" FROM t",
            "SELECT 'open",
            "SELECT /* open",
            "SELECT $x$ open",
            "SELECT \"open",
            "SELECT 1_000, 1.5e3, x$y FROM t",
            "SELECT 'café', \"naïve\"",
        ];
        let mut admitted = 0;
        for sql in corpus {
            if check_unambiguous(&[sql.to_string()]).is_ok() {
                admitted += 1;
                assert_eq!(quoted_and_comment_spans(sql), lex(sql, true).spans, "{sql}");
                assert_eq!(
                    quoted_and_comment_spans(sql),
                    lex(sql, false).spans,
                    "{sql}"
                );
            }
        }
        // Not vacuous: the ordinary forms are admitted.
        assert!(admitted >= 12, "only {admitted} admitted");
    }

    #[test]
    fn a_split_statement_never_holds_a_top_level_semicolon() {
        // The splitter would never produce these, but a statement handed over unsplit must not
        // carry a second one.
        let m = check_unambiguous(&["SELECT 1; DROP TABLE t".to_string()])
            .unwrap_err()
            .to_string();
        assert!(m.contains("';'"), "{m}");
        check_unambiguous(&["SELECT ';'".to_string()]).unwrap();
        check_unambiguous(&[
            "CREATE PROCEDURE p() AS $$ BEGIN NULL; END; $$ LANGUAGE plpgsql".to_string(),
        ])
        .unwrap();
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
    fn read_only_admits_queries_built_ins_and_show_forms() {
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
            "SELECT count(*), sum(x), date_trunc('day', at), lower(name) FROM s GROUP BY 3, 4",
            "SELECT CAST(x AS varchar(20)), x::numeric(12, 2), pg_catalog.lower(n) FROM t",
            "SELECT row_number() OVER (PARTITION BY a ORDER BY b) FROM t WHERE a IN (1, 2)",
            "SELECT json_extract_path_text(doc, 'a'), COALESCE(a, b) FROM t \
             WHERE EXISTS (SELECT 1 FROM u)",
            "SELECT \"name\" FROM t",
        ] {
            check_read_only(&stmts(sql)).unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
    }

    /// HIGH-adjacent finding 2: nothing on Redshift stops a function the classifier does not
    /// know, so a read-only run refuses every call that is not a read-only built-in.
    #[test]
    fn read_only_refuses_unknown_and_schema_qualified_functions() {
        for (sql, f) in [
            ("SELECT f_notify(card_number) FROM payments", "f_notify()"),
            ("SELECT public.f(x) FROM t", "public.f()"),
            ("SELECT myschema.lower(x) FROM t", "myschema.lower()"),
            ("SELECT \"LOWER\"(x) FROM t", "LOWER()"),
            (
                "SELECT change_query_priority(1, 'high')",
                "change_query_priority()",
            ),
            (
                "SELECT change_session_priority(1, 'high')",
                "change_session_priority()",
            ),
            (
                "SELECT change_user_priority('u', 'high')",
                "change_user_priority()",
            ),
            ("SELECT reboot_cluster()", "reboot_cluster()"),
            ("SELECT x FROM generate_series(1, 3)", "generate_series()"),
        ] {
            let m = check_read_only(&stmts(sql)).unwrap_err().to_string();
            // The admin functions are on the general deny-list too, so the classifier already
            // names them; everything else gets the built-in allow-list's message.
            assert!(m.contains(f), "{sql}: {m}");
            if !f.starts_with("change_") && !f.starts_with("reboot") {
                assert!(m.contains("with approval"), "{sql}: {m}");
            }
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
