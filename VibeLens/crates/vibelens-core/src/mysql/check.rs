//! What a MySQL statement is, decided from its text before anything is sent.
//!
//! - [`split_script`]: splits a script on top-level `;` with the MySQL lexer ([`super::lex`]),
//!   keeping `BEGIN … END` bodies of `CREATE PROCEDURE/FUNCTION/TRIGGER/EVENT` whole. Each piece
//!   is sent as its own `COM_QUERY`; the driver does not advertise `CLIENT_MULTI_STATEMENTS`, so
//!   a piece that still holds two statements fails on the server instead of running both.
//! - [`check_read_only`]: the pre-submit check of a read-only run (agent reads, read-only editor
//!   runs). It fails closed: a statement passes only when it is provably a read. The run itself
//!   is also inside `START TRANSACTION READ ONLY`, but that transaction does not stop
//!   `SELECT … INTO OUTFILE`, `LOAD_FILE()`, `SLEEP()`/`BENCHMARK()`, `GET_LOCK()`, user
//!   variable assignment, `SET GLOBAL`, `KILL`, `HANDLER`, `DO`, `CALL` or a loadable function
//!   (UDF) with side effects, so this check refuses all of them, and every function call that is
//!   not a known read-only built-in.
//! - [`classify`]: per-statement classes for the agent policy and the production write guard.
//! - [`agent_refusal`]: statements the agent may not run even with approval.

use super::lex::{Kind, Tok, is_comment, lex, lex_unambiguous, text, upper};
use crate::sql::{SqlClassification, StatementClass};

// ---- splitting ---------------------------------------------------------------------------------

/// Statement kinds whose body may be a `BEGIN … END` block with `;` inside.
const ROUTINE_OBJECTS: &[&str] = &["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT"];

/// Whether the words so far start a statement with a compound body: `CREATE [OR REPLACE]
/// [DEFINER = …] [AGGREGATE] PROCEDURE|FUNCTION|TRIGGER|EVENT` or `ALTER EVENT`.
fn starts_routine(words: &[String]) -> bool {
    match words.first().map(String::as_str) {
        Some("CREATE") => words
            .iter()
            .skip(1)
            .take(8)
            .any(|w| ROUTINE_OBJECTS.contains(&w.as_str())),
        Some("ALTER") => words.get(1).is_some_and(|w| w == "EVENT"),
        _ => false,
    }
}

/// Splits `sql` into statements (trimmed, without the `;`), dropping empty ones. Text that
/// cannot be lexed (an unterminated string or comment) is left in the last statement as it is,
/// for the server to report.
pub fn split_script(sql: &str) -> Vec<String> {
    let toks = match lex(sql, true) {
        Ok(t) => t,
        Err(_) => {
            // Split what lexes; the rest is one statement.
            return split_lenient(sql);
        }
    };
    split_tokens(sql, &toks)
}

fn split_lenient(sql: &str) -> Vec<String> {
    // Find the longest prefix that lexes and ends at a statement boundary; everything after
    // the last boundary before the error stays together.
    let mut out = Vec::new();
    let mut rest = sql;
    loop {
        match lex(rest, true) {
            Ok(toks) => {
                out.extend(split_tokens(rest, &toks));
                return out;
            }
            Err(_) => {
                // Cut at the first top-level `;` that is lexed cleanly before the error.
                let mut cut = None;
                let b = rest.as_bytes();
                for (i, &c) in b.iter().enumerate() {
                    if c == b';' && lex(&rest[..=i], true).is_ok() {
                        let toks = lex(&rest[..=i], true).unwrap_or_default();
                        if split_tokens(&rest[..=i], &toks).len() == 1
                            && toks.last().is_some_and(|t| t.kind == Kind::Punct)
                        {
                            cut = Some(i);
                            break;
                        }
                    }
                }
                match cut {
                    Some(i) => {
                        let head = rest[..i].trim();
                        if !head.is_empty() {
                            out.push(head.to_string());
                        }
                        rest = &rest[i + 1..];
                    }
                    None => {
                        let t = rest.trim();
                        if !t.is_empty() {
                            out.push(t.to_string());
                        }
                        return out;
                    }
                }
            }
        }
    }
}

fn split_tokens(sql: &str, toks: &[Tok]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut words: Vec<String> = Vec::new();
    let mut routine = false;
    let mut depth = 0usize;
    let mut i = 0;
    let push = |out: &mut Vec<String>, piece: &str| {
        let t = piece.trim();
        if !t.is_empty() && t.chars().any(|c| !c.is_whitespace() && c != ';') {
            out.push(t.to_string());
        }
    };
    while i < toks.len() {
        let t = &toks[i];
        match t.kind {
            Kind::Word => {
                let w = upper(sql, t);
                if words.len() < 10 {
                    words.push(w.clone());
                    if !routine {
                        routine = starts_routine(&words);
                    }
                }
                if routine {
                    match w.as_str() {
                        "BEGIN" | "CASE" => depth += 1,
                        "END" => {
                            let next = toks[i + 1..]
                                .iter()
                                .find(|n| !is_comment(n))
                                .filter(|n| n.kind == Kind::Word)
                                .map(|n| upper(sql, n));
                            match next.as_deref() {
                                Some("IF" | "LOOP" | "WHILE" | "REPEAT") => {}
                                _ => depth = depth.saturating_sub(1),
                            }
                        }
                        _ => {}
                    }
                }
            }
            Kind::Punct if text(sql, t) == ";" && depth == 0 => {
                push(&mut out, &sql[start..t.start]);
                start = t.end;
                words.clear();
                routine = false;
            }
            _ => {}
        }
        i += 1;
    }
    push(&mut out, &sql[start..]);
    out
}

// ---- read-only check ---------------------------------------------------------------------------

/// Bare words that never appear in a read: statements that write, change the session or the
/// server, lock, or run code. A read that needs one as a name can quote it with backticks.
const DENY_WORDS: &[&str] = &[
    "ALTER",
    "ANALYZE",
    "BEGIN",
    "BINLOG",
    "CACHE",
    "CALL",
    "CHANGE",
    "CLONE",
    "COMMIT",
    "CREATE",
    "DEALLOCATE",
    "DECLARE",
    "DELETE",
    "DELIMITER",
    "DO",
    "DROP",
    "DUMPFILE",
    "EXECUTE",
    "FLUSH",
    "GRANT",
    "HANDLER",
    "IMPORT",
    "INSERT",
    "INSTALL",
    "INTO",
    "KILL",
    "LOAD",
    "LOCK",
    "OPTIMIZE",
    "OUTFILE",
    "PREPARE",
    "PROCEDURE",
    "PURGE",
    "RELEASE",
    "RENAME",
    "REPAIR",
    "REPLACE",
    "RESET",
    "RESIGNAL",
    "RESTART",
    "REVOKE",
    "ROLLBACK",
    "SAVEPOINT",
    "SET",
    "SHUTDOWN",
    "SIGNAL",
    "START",
    "TRUNCATE",
    "UNINSTALL",
    "UNLOCK",
    "UPDATE",
    "USE",
    "XA",
];

/// Deny words that are also built-in functions; as `word(` they are function calls.
const DENY_WORDS_AS_FUNCTIONS: &[&str] = &["INSERT", "REPLACE", "TRUNCATE"];

/// Keywords that may be followed by `(` in a read without that being a function call.
const PAREN_KEYWORDS: &[&str] = &[
    "AGAINST",
    "ALL",
    "AND",
    "ANY",
    "ARRAY",
    "AS",
    "BETWEEN",
    "BIGINT",
    "BINARY",
    "BIT",
    "BY",
    "CASE",
    "CHAR",
    "CHARACTER",
    "COLUMNS",
    "DATE",
    "DATETIME",
    "DEC",
    "DECIMAL",
    "DISTINCT",
    "DIV",
    "DOUBLE",
    "ELSE",
    "ENUM",
    "EXCEPT",
    "EXISTS",
    "FIXED",
    "FLOAT",
    "FORCE",
    "FROM",
    "HAVING",
    "IGNORE",
    "IN",
    "INDEX",
    "INT",
    "INTEGER",
    "INTERSECT",
    "INTERVAL",
    "IS",
    "JOIN",
    "JSON",
    "KEY",
    "LATERAL",
    "LIKE",
    "MEDIUMINT",
    "MEMBER",
    "MOD",
    "NCHAR",
    "NESTED",
    "NOT",
    "NUMERIC",
    "NVARCHAR",
    "OF",
    "ON",
    "OR",
    "OVER",
    "PARTITION",
    "PATH",
    "REAL",
    "REGEXP",
    "RLIKE",
    "ROW",
    "SELECT",
    "SIGNED",
    "SMALLINT",
    "SOME",
    "THEN",
    "TIME",
    "TIMESTAMP",
    "TINYINT",
    "UNION",
    "UNSIGNED",
    "USING",
    "VALUES",
    "VARBINARY",
    "VARCHAR",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "XOR",
    "YEAR",
];

/// MySQL 8 built-in functions that only compute a value from their arguments or read session,
/// server or catalog information. Left out on purpose: `SLEEP`, `BENCHMARK` (tie up the
/// server), `GET_LOCK`, `RELEASE_LOCK`, `RELEASE_ALL_LOCKS`, `IS_FREE_LOCK`, `IS_USED_LOCK`
/// (locks), `LOAD_FILE` (reads a server file), `LAST_INSERT_ID` (sets a session value when given
/// an argument), `MASTER_POS_WAIT`, `SOURCE_POS_WAIT`, `WAIT_FOR_EXECUTED_GTID_SET` (block), and
/// every stored or loadable function: those are not built-ins, and nothing in a read-only
/// transaction stops what a loadable function does.
const READ_ONLY_FUNCTIONS: &[&str] = &[
    // aggregate and window
    "ANY_VALUE",
    "AVG",
    "BIT_AND",
    "BIT_OR",
    "BIT_XOR",
    "COUNT",
    "CUME_DIST",
    "DENSE_RANK",
    "FIRST_VALUE",
    "GROUPING",
    "GROUP_CONCAT",
    "JSON_ARRAYAGG",
    "JSON_OBJECTAGG",
    "LAG",
    "LAST_VALUE",
    "LEAD",
    "MAX",
    "MIN",
    "NTH_VALUE",
    "NTILE",
    "PERCENT_RANK",
    "RANK",
    "ROW_NUMBER",
    "STD",
    "STDDEV",
    "STDDEV_POP",
    "STDDEV_SAMP",
    "SUM",
    "VARIANCE",
    "VAR_POP",
    "VAR_SAMP",
    // control flow and comparison
    "CASE",
    "COALESCE",
    "GREATEST",
    "IF",
    "IFNULL",
    "INTERVAL",
    "ISNULL",
    "LEAST",
    "NULLIF",
    "STRCMP",
    // string
    "ASCII",
    "BIN",
    "BIT_LENGTH",
    "CHAR",
    "CHARACTER_LENGTH",
    "CHAR_LENGTH",
    "CONCAT",
    "CONCAT_WS",
    "ELT",
    "EXPORT_SET",
    "FIELD",
    "FIND_IN_SET",
    "FORMAT",
    "FROM_BASE64",
    "HEX",
    "INSERT",
    "INSTR",
    "LCASE",
    "LEFT",
    "LENGTH",
    "LOCATE",
    "LOWER",
    "LPAD",
    "LTRIM",
    "MAKE_SET",
    "MATCH",
    "MID",
    "OCT",
    "OCTET_LENGTH",
    "ORD",
    "POSITION",
    "QUOTE",
    "REGEXP_INSTR",
    "REGEXP_LIKE",
    "REGEXP_REPLACE",
    "REGEXP_SUBSTR",
    "REPEAT",
    "REPLACE",
    "REVERSE",
    "RIGHT",
    "RPAD",
    "RTRIM",
    "SOUNDEX",
    "SPACE",
    "SUBSTR",
    "SUBSTRING",
    "SUBSTRING_INDEX",
    "TO_BASE64",
    "TRIM",
    "UCASE",
    "UNHEX",
    "UPPER",
    "WEIGHT_STRING",
    // numeric
    "ABS",
    "ACOS",
    "ASIN",
    "ATAN",
    "ATAN2",
    "BIT_COUNT",
    "CEIL",
    "CEILING",
    "CONV",
    "COS",
    "COT",
    "CRC32",
    "DEGREES",
    "EXP",
    "FLOOR",
    "LN",
    "LOG",
    "LOG10",
    "LOG2",
    "MOD",
    "PI",
    "POW",
    "POWER",
    "RADIANS",
    "RAND",
    "ROUND",
    "SIGN",
    "SIN",
    "SQRT",
    "TAN",
    "TRUNCATE",
    // date and time
    "ADDDATE",
    "ADDTIME",
    "CONVERT_TZ",
    "CURDATE",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURTIME",
    "DATE",
    "DATEDIFF",
    "DATE_ADD",
    "DATE_FORMAT",
    "DATE_SUB",
    "DAY",
    "DAYNAME",
    "DAYOFMONTH",
    "DAYOFWEEK",
    "DAYOFYEAR",
    "EXTRACT",
    "FROM_DAYS",
    "FROM_UNIXTIME",
    "GET_FORMAT",
    "HOUR",
    "LAST_DAY",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "MAKEDATE",
    "MAKETIME",
    "MICROSECOND",
    "MINUTE",
    "MONTH",
    "MONTHNAME",
    "NOW",
    "PERIOD_ADD",
    "PERIOD_DIFF",
    "QUARTER",
    "SECOND",
    "SEC_TO_TIME",
    "STR_TO_DATE",
    "SUBDATE",
    "SUBTIME",
    "SYSDATE",
    "TIME",
    "TIMEDIFF",
    "TIMESTAMP",
    "TIMESTAMPADD",
    "TIMESTAMPDIFF",
    "TIME_FORMAT",
    "TIME_TO_SEC",
    "TO_DAYS",
    "TO_SECONDS",
    "UNIX_TIMESTAMP",
    "UTC_DATE",
    "UTC_TIME",
    "UTC_TIMESTAMP",
    "WEEK",
    "WEEKDAY",
    "WEEKOFYEAR",
    "YEAR",
    "YEARWEEK",
    // cast, conversion, character sets
    "BINARY",
    "CAST",
    "CHARSET",
    "COERCIBILITY",
    "COLLATION",
    "CONVERT",
    // JSON
    "JSON_ARRAY",
    "JSON_ARRAY_APPEND",
    "JSON_ARRAY_INSERT",
    "JSON_CONTAINS",
    "JSON_CONTAINS_PATH",
    "JSON_DEPTH",
    "JSON_EXTRACT",
    "JSON_INSERT",
    "JSON_KEYS",
    "JSON_LENGTH",
    "JSON_MERGE",
    "JSON_MERGE_PATCH",
    "JSON_MERGE_PRESERVE",
    "JSON_OBJECT",
    "JSON_OVERLAPS",
    "JSON_PRETTY",
    "JSON_QUOTE",
    "JSON_REMOVE",
    "JSON_REPLACE",
    "JSON_SCHEMA_VALID",
    "JSON_SCHEMA_VALIDATION_REPORT",
    "JSON_SEARCH",
    "JSON_SET",
    "JSON_STORAGE_FREE",
    "JSON_STORAGE_SIZE",
    "JSON_TABLE",
    "JSON_TYPE",
    "JSON_UNQUOTE",
    "JSON_VALID",
    "JSON_VALUE",
    "MEMBER",
    // spatial (a selection; all compute on their arguments)
    "POINT",
    "LINESTRING",
    "POLYGON",
    "MULTIPOINT",
    "MULTILINESTRING",
    "MULTIPOLYGON",
    "GEOMETRYCOLLECTION",
    "ST_AREA",
    "ST_ASBINARY",
    "ST_ASGEOJSON",
    "ST_ASTEXT",
    "ST_ASWKB",
    "ST_ASWKT",
    "ST_BUFFER",
    "ST_CENTROID",
    "ST_CONTAINS",
    "ST_CROSSES",
    "ST_DISJOINT",
    "ST_DISTANCE",
    "ST_DISTANCE_SPHERE",
    "ST_ENDPOINT",
    "ST_ENVELOPE",
    "ST_EQUALS",
    "ST_GEOMETRYTYPE",
    "ST_GEOMFROMGEOJSON",
    "ST_GEOMFROMTEXT",
    "ST_GEOMFROMWKB",
    "ST_INTERSECTION",
    "ST_INTERSECTS",
    "ST_ISEMPTY",
    "ST_ISVALID",
    "ST_LATITUDE",
    "ST_LENGTH",
    "ST_LONGITUDE",
    "ST_NUMPOINTS",
    "ST_OVERLAPS",
    "ST_POINTFROMTEXT",
    "ST_SRID",
    "ST_STARTPOINT",
    "ST_TOUCHES",
    "ST_UNION",
    "ST_WITHIN",
    "ST_X",
    "ST_Y",
    // hashing, encoding, compression
    "AES_DECRYPT",
    "AES_ENCRYPT",
    "COMPRESS",
    "MD5",
    "RANDOM_BYTES",
    "SHA",
    "SHA1",
    "SHA2",
    "STATEMENT_DIGEST",
    "STATEMENT_DIGEST_TEXT",
    "UNCOMPRESS",
    "UNCOMPRESSED_LENGTH",
    // information
    "BIN_TO_UUID",
    "CONNECTION_ID",
    "CURRENT_ROLE",
    "CURRENT_USER",
    "DATABASE",
    "FORMAT_BYTES",
    "FORMAT_PICO_TIME",
    "FOUND_ROWS",
    "ICU_VERSION",
    "INET6_ATON",
    "INET6_NTOA",
    "INET_ATON",
    "INET_NTOA",
    "IS_IPV4",
    "IS_IPV4_COMPAT",
    "IS_IPV4_MAPPED",
    "IS_IPV6",
    "IS_UUID",
    "PS_CURRENT_THREAD_ID",
    "PS_THREAD_ID",
    "ROLES_GRAPHML",
    "ROW_COUNT",
    "SCHEMA",
    "SESSION_USER",
    "SYSTEM_USER",
    "USER",
    "UUID",
    "UUID_SHORT",
    "UUID_TO_BIN",
    "VERSION",
    // misc
    "DEFAULT",
    "NAME_CONST",
    "VALUES",
];

/// The name a word, a backtick-quoted identifier or a double-quoted one (ANSI_QUOTES) spells.
fn name_of(sql: &str, t: &Tok) -> Option<String> {
    let raw = text(sql, t);
    match t.kind {
        Kind::Word => Some(raw.to_string()),
        Kind::Backtick if raw.len() >= 2 => Some(raw[1..raw.len() - 1].replace("``", "`")),
        Kind::Str if raw.starts_with('"') && raw.len() >= 2 => {
            Some(raw[1..raw.len() - 1].replace("\"\"", "\""))
        }
        _ => None,
    }
}

fn is_word(sql: &str, t: &Tok, w: &str) -> bool {
    t.kind == Kind::Word && text(sql, t).eq_ignore_ascii_case(w)
}

/// Tokens without comments (the caller has already refused the comments that matter).
fn significant(toks: &[Tok]) -> Vec<Tok> {
    toks.iter().copied().filter(|t| !is_comment(t)).collect()
}

/// Index of the `)` that closes the `(` at `open`, if any.
fn matching_paren(sql: &str, toks: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate().skip(open) {
        if t.kind == Kind::Punct {
            match text(sql, t) {
                "(" => depth += 1,
                ")" => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// The first call in `toks` to something that is not a read-only built-in, as written.
fn first_unknown_call(sql: &str, toks: &[Tok]) -> Option<String> {
    for i in 0..toks.len() {
        let t = &toks[i];
        // Under ANSI_QUOTES `"name"` is an identifier, so `"SLEEP"(1)` is a call; in any other
        // mode it is a syntax error. Either way a double-quoted name before `(` is a call here.
        let dquoted = t.kind == Kind::Str && text(sql, t).starts_with('"');
        if !(matches!(t.kind, Kind::Word | Kind::Backtick) || dquoted) {
            continue;
        }
        let Some(next) = toks.get(i + 1) else {
            continue;
        };
        if !(next.kind == Kind::Punct && text(sql, next) == "(") {
            continue;
        }
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        // `AS name(…)`: a type in CAST/CONVERT, or a derived table's column list; never a call.
        if prev.is_some_and(|p| is_word(sql, p, "AS")) {
            continue;
        }
        // `WITH name(col, …) AS (`: a common table expression's column list.
        if let Some(close) = matching_paren(sql, toks, i + 1)
            && toks.get(close + 1).is_some_and(|a| is_word(sql, a, "AS"))
            && toks
                .get(close + 2)
                .is_some_and(|p| p.kind == Kind::Punct && text(sql, p) == "(")
        {
            continue;
        }
        let name = text(sql, t);
        // `db.func(…)`: a stored function.
        if prev.is_some_and(|p| p.kind == Kind::Punct && text(sql, p) == ".") {
            return Some(format!("a stored function ({name})"));
        }
        if t.kind != Kind::Word {
            return Some(format!("a stored or loadable function ({name})"));
        }
        let up = name.to_ascii_uppercase();
        if PAREN_KEYWORDS.contains(&up.as_str()) || READ_ONLY_FUNCTIONS.contains(&up.as_str()) {
            continue;
        }
        return Some(format!("{up}()"));
    }
    None
}

/// Why `stmt` is not a provable read, or `Ok` when it is. This is the gate of every read-only
/// run: a statement is sent only when this returns `Ok`.
pub fn check_read_only(stmt: &str) -> Result<(), String> {
    for c in stmt.chars() {
        if (c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\x0B' | '\x0C'))
            || matches!(c, '\u{0}')
        {
            return Err(format!(
                "the statement contains the control character U+{:04X}",
                c as u32
            ));
        }
    }
    let toks = lex_unambiguous(stmt)?;
    for t in &toks {
        match t.kind {
            Kind::ExecComment => {
                return Err(
                    "it contains an executable comment (/*! … */), which the server runs".into(),
                );
            }
            Kind::Hint => {
                return Err(
                    "it contains an optimizer hint (/*+ … */), which can change session \
                     settings (SET_VAR)"
                        .into(),
                );
            }
            Kind::Assign => {
                return Err("it assigns a user variable (:=)".into());
            }
            // The server ends `#` and `-- ` comments at `\n` only. A `\r` inside one is where
            // two readers could disagree on what is comment and what is code; not worth it.
            Kind::LineComment if text(stmt, t).contains('\r') => {
                return Err("a line comment contains a carriage return (\\r)".into());
            }
            Kind::Word if !text(stmt, t).is_ascii() => {
                return Err(
                    "it has a name with non-ASCII characters outside backticks; quote it with \
                     backticks"
                        .into(),
                );
            }
            Kind::Punct => match text(stmt, t) {
                ";" => return Err("it holds more than one statement".into()),
                "\\" => return Err("it has a backslash outside a string".into()),
                p if !p.is_ascii() => {
                    return Err("it has a non-ASCII character outside quotes".into());
                }
                _ => {}
            },
            _ => {}
        }
    }
    let toks = significant(&toks);
    // Skip leading `(` (`(SELECT …) UNION (SELECT …)`).
    let first = toks
        .iter()
        .find(|t| !(t.kind == Kind::Punct && text(stmt, t) == "("));
    let Some(first) = first else {
        return Err("it is empty".into());
    };
    if first.kind != Kind::Word {
        return Err("it does not start with a keyword".into());
    }
    let lead = upper(stmt, first);
    let is_show = lead == "SHOW";
    match lead.as_str() {
        "SELECT" | "WITH" | "TABLE" | "VALUES" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" => {}
        other => return Err(format!("{other} is not a read")),
    }
    if is_show {
        let words: Vec<String> = toks
            .iter()
            .filter(|t| t.kind == Kind::Word)
            .take(3)
            .map(|t| upper(stmt, t))
            .collect();
        if words.get(1).map(String::as_str) == Some("CREATE")
            && words.get(2).map(String::as_str) == Some("USER")
        {
            return Err("SHOW CREATE USER shows password hashes".into());
        }
    }
    for (i, t) in toks.iter().enumerate() {
        if t.kind != Kind::Word {
            continue;
        }
        let w = upper(stmt, t);
        let prev = i.checked_sub(1).map(|p| upper(stmt, &toks[p]));
        let next = toks.get(i + 1);
        let next_word = next
            .filter(|n| n.kind == Kind::Word)
            .map(|n| upper(stmt, n));
        let next_is_paren = next.is_some_and(|n| n.kind == Kind::Punct && text(stmt, n) == "(");
        // SHOW statements name objects with words such as CREATE (SHOW CREATE TABLE) and
        // PROCEDURE (SHOW PROCEDURE STATUS); only what can do something is refused there.
        // Before the SHOW branch: a locking read is refused wherever it appears.
        if w == "FOR"
            && matches!(
                next_word.as_deref(),
                Some("UPDATE" | "SHARE" | "CONNECTION")
            )
        {
            return Err(format!(
                "FOR {} {}",
                next_word.unwrap_or_default(),
                "locks rows or reads another session"
            ));
        }
        if is_show {
            // A subquery inside SHOW … WHERE is not needed to read the catalog, and would get
            // only the SHOW-specific checks: refused.
            if matches!(
                w.as_str(),
                "INTO" | "OUTFILE" | "DUMPFILE" | "SET" | "LOCK" | "SELECT" | "WITH"
            ) {
                return Err(format!("{w} is not part of a SHOW read"));
            }
            continue;
        }
        if !DENY_WORDS.contains(&w.as_str()) {
            continue;
        }
        if DENY_WORDS_AS_FUNCTIONS.contains(&w.as_str()) && next_is_paren {
            continue;
        }
        // Index hints: `USE INDEX (…)`, `USE KEY (…)`.
        if w == "USE" && matches!(next_word.as_deref(), Some("INDEX" | "KEY")) {
            continue;
        }
        // `CAST(x AS CHAR CHARACTER SET utf8mb4)`.
        if w == "SET" && matches!(prev.as_deref(), Some("CHARACTER" | "CHAR")) {
            continue;
        }
        // `EXPLAIN ANALYZE SELECT …` runs the SELECT, which this check covers.
        if w == "ANALYZE" && i > 0 && prev.as_deref() == Some("EXPLAIN") {
            continue;
        }
        return Err(format!("{w} is not part of a read"));
    }
    if let Some(call) = first_unknown_call(stmt, &toks) {
        return Err(format!(
            "it calls {call}, which is not a built-in function known to only read"
        ));
    }
    Ok(())
}

// ---- classification ------------------------------------------------------------------------

/// First words of `stmt` (upper-cased), comments skipped, leading `(` skipped.
fn leading_words(stmt: &str, max: usize) -> Vec<String> {
    let Ok(toks) = lex(stmt, true) else {
        return Vec::new();
    };
    toks.iter()
        .filter(|t| !is_comment(t))
        .skip_while(|t| t.kind == Kind::Punct && text(stmt, t) == "(")
        .take_while(|t| t.kind == Kind::Word)
        .take(max)
        .map(|t| upper(stmt, t))
        .collect()
}

fn all_words(stmt: &str) -> Vec<String> {
    lex(stmt, true)
        .map(|toks| {
            toks.iter()
                .filter(|t| t.kind == Kind::Word)
                .map(|t| upper(stmt, t))
                .collect()
        })
        .unwrap_or_default()
}

/// `SET GLOBAL …`, `SET PERSIST …`, `SET @@global.…`: changes the server, not the session.
fn sets_server_variable(stmt: &str) -> bool {
    let Ok(toks) = lex(stmt, true) else {
        return false;
    };
    let toks = significant(&toks);
    if !toks.first().is_some_and(|t| is_word(stmt, t, "SET")) {
        return false;
    }
    toks.iter().any(|t| match t.kind {
        Kind::Word => matches!(
            upper(stmt, t).as_str(),
            "GLOBAL" | "PERSIST" | "PERSIST_ONLY"
        ),
        Kind::Variable => {
            let v = text(stmt, t).to_ascii_lowercase();
            v.starts_with("@@global.")
                || v.starts_with("@@persist.")
                || v.starts_with("@@persist_only.")
        }
        _ => false,
    })
}

/// The class and a short label of one statement.
fn classify_statement(stmt: &str) -> (StatementClass, String) {
    if lex(stmt, true).is_err() {
        return (StatementClass::Unknown, "unreadable statement".into());
    }
    let w = leading_words(stmt, 4);
    let first = w.first().cloned().unwrap_or_default();
    let label = |n: usize| w.iter().take(n).cloned().collect::<Vec<_>>().join(" ");
    if check_read_only(stmt).is_ok() {
        return (StatementClass::ReadOnly, first);
    }
    let words = all_words(stmt);
    let has = |x: &str| words.iter().any(|w| w == x);
    match first.as_str() {
        "SELECT" | "WITH" | "TABLE" | "VALUES" | "EXPLAIN" | "DESCRIBE" | "DESC" | "SHOW" => {
            if has("OUTFILE") || has("DUMPFILE") {
                (
                    StatementClass::Unknown,
                    format!("{first} INTO OUTFILE (writes a file on the server)"),
                )
            } else if ["UPDATE", "DELETE", "INSERT", "REPLACE"]
                .iter()
                .any(|x| w.iter().skip(1).any(|y| y == x) || (first == "WITH" && has(x)))
                && first != "SELECT"
            {
                (StatementClass::Write, label(1))
            } else if has("LOCK") || (has("FOR") && (has("UPDATE") || has("SHARE"))) {
                (StatementClass::Write, format!("{first} (locking read)"))
            } else {
                (StatementClass::Unknown, first)
            }
        }
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "LOAD" | "IMPORT" => {
            (StatementClass::Write, first)
        }
        "CREATE" | "ALTER" | "DROP" => (StatementClass::Ddl, label(2)),
        "TRUNCATE" | "RENAME" | "GRANT" | "REVOKE" | "ANALYZE" | "OPTIMIZE" | "REPAIR"
        | "CHECK" | "CHECKSUM" | "FLUSH" | "INSTALL" | "UNINSTALL" | "PURGE" | "RESET"
        | "CHANGE" | "CACHE" => (StatementClass::Ddl, first),
        "SET" if sets_server_variable(stmt) => (
            StatementClass::Ddl,
            "SET GLOBAL (server configuration)".into(),
        ),
        // Accounts, roles and resource groups are server state, not session state.
        "SET"
            if matches!(
                w.get(1).map(String::as_str),
                Some("PASSWORD" | "DEFAULT" | "ROLE" | "RESOURCE")
            ) =>
        {
            (StatementClass::Ddl, label(3))
        }
        // START REPLICA, START SLAVE, START GROUP_REPLICATION: server administration.
        "START" if w.get(1).map(String::as_str) != Some("TRANSACTION") => {
            (StatementClass::Ddl, label(2))
        }
        // LOCK TABLES … WRITE blocks other sessions; LOCK INSTANCE FOR BACKUP blocks DDL
        // server-wide; XA COMMIT/ROLLBACK 'xid' ends a transaction prepared by another session.
        "LOCK" | "XA" => (StatementClass::Write, label(2)),
        "SET" | "USE" | "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE"
        | "UNLOCK" => (StatementClass::Session, label(2)),
        "" => (StatementClass::Unknown, "statement".into()),
        // CALL, DO, HANDLER, KILL, PREPARE/EXECUTE, SHUTDOWN, HELP, … : could do anything.
        _ => (StatementClass::Unknown, first),
    }
}

/// Classes of every statement of `sql` (split with [`split_script`]), the most dangerous one
/// overall, and a summary such as `2 statements: SELECT, DROP TABLE`.
pub fn classify(sql: &str) -> SqlClassification {
    let statements = split_script(sql);
    let items: Vec<(StatementClass, String)> =
        statements.iter().map(|s| classify_statement(s)).collect();
    let parse_ok = statements.iter().all(|s| lex(s, true).is_ok());
    let overall = items
        .iter()
        .map(|(c, _)| *c)
        .max()
        .unwrap_or(StatementClass::ReadOnly);
    let labels: Vec<&str> = items.iter().map(|(_, l)| l.as_str()).collect();
    let summary = match labels.len() {
        0 => "empty script".to_string(),
        1 => labels[0].to_string(),
        n => format!("{n} statements: {}", labels.join(", ")),
    };
    SqlClassification {
        statements: items.into_iter().map(|(c, _)| c).collect(),
        overall,
        parse_ok,
        summary,
    }
}

/// `DELIMITER` is a command of the `mysql` command-line client, not SQL: the server would
/// reject it, and it changes how the client splits what follows. Refused in every mode with
/// advice, before anything runs.
pub fn check_no_delimiter(statements: &[String]) -> Result<(), String> {
    if statements.iter().any(|s| {
        leading_words(s, 1)
            .first()
            .is_some_and(|w| w == "DELIMITER")
    }) {
        return Err(
            "DELIMITER is a command of the mysql command-line client, not SQL. Remove the \
             DELIMITER lines and end each statement with ';': VibeLens keeps BEGIN … END \
             bodies of CREATE PROCEDURE, FUNCTION, TRIGGER and EVENT together itself"
                .into(),
        );
    }
    Ok(())
}

/// Why the agent may not run `sql` at all, even with approval, or `None`. These reach outside
/// the database (server files, the server's configuration, other sessions, loadable code), or
/// would make the approval card read differently from what runs.
pub fn agent_refusal(sql: &str) -> Option<String> {
    let statements = split_script(sql);
    if let Err(e) = check_no_delimiter(&statements) {
        return Some(e);
    }
    for stmt in &statements {
        // What cannot be read one way only (unterminated, or a backslash that moves a string's
        // end under NO_BACKSLASH_ESCAPES) cannot be checked, so it is not run.
        let toks = match lex_unambiguous(stmt) {
            Ok(t) => t,
            Err(e) => {
                return Some(format!(
                    "the statement cannot be read one way only ({e}), so what it does cannot be \
                     checked"
                ));
            }
        };
        if toks.iter().any(|t| t.kind == Kind::ExecComment) {
            return Some(
                "the statement contains an executable comment (/*! … */): the server runs what \
                 is inside it, so the text shown for approval would not read as what runs"
                    .into(),
            );
        }
        let toks = significant(&toks);
        let words: Vec<String> = toks
            .iter()
            .filter(|t| t.kind == Kind::Word)
            .map(|t| upper(stmt, t))
            .collect();
        let first = words.first().map(String::as_str).unwrap_or("");
        let has = |x: &str| words.iter().any(|w| w == x);
        let reason = if has("OUTFILE") || has("DUMPFILE") {
            Some("SELECT … INTO OUTFILE/DUMPFILE writes a file on the database server")
        } else if first == "LOAD" {
            Some("LOAD DATA and LOAD XML read files on the database server or on this machine")
        } else if matches!(first, "PREPARE" | "EXECUTE" | "DEALLOCATE") {
            Some(
                "PREPARE and EXECUTE run SQL built from a string or a variable, so the statement \
                 shown for approval is not the one that runs; send the statement itself",
            )
        } else if toks.iter().enumerate().any(|(i, t)| {
            name_of(stmt, t).is_some_and(|n| n.eq_ignore_ascii_case("LOAD_FILE"))
                && toks
                    .get(i + 1)
                    .is_some_and(|n| n.kind == Kind::Punct && text(stmt, n) == "(")
        }) {
            Some("LOAD_FILE() reads a file on the database server")
        } else if first == "SET" && sets_server_variable(stmt) {
            Some("SET GLOBAL and SET PERSIST change the server's configuration for everyone")
        } else if matches!(first, "KILL" | "SHUTDOWN" | "RESTART" | "CLONE") {
            Some(
                "server administration (KILL, SHUTDOWN, RESTART, CLONE) is not available to the agent",
            )
        } else if matches!(first, "INSTALL" | "UNINSTALL") || has("SONAME") {
            Some(
                "installing plugins, components or loadable functions runs native code in the server",
            )
        } else if first == "USE" {
            Some(
                "USE changes the database of the session; pass the database as the run_sql \
                 `database` argument (the approval card names it) or qualify names as db.table",
            )
        } else {
            None
        };
        if let Some(r) = reason {
            return Some(r.to_string());
        }
    }
    None
}

/// The command tag shown for a statement without a result set (`INSERT`, `CREATE TABLE`, …).
pub(crate) fn command_tag(stmt: &str) -> Option<String> {
    let w = leading_words(stmt, 6);
    let first = w.first()?.as_str();
    Some(match first {
        "CREATE" | "ALTER" | "DROP" => {
            let rest = w[1..]
                .iter()
                .find(|x| {
                    !matches!(
                        x.as_str(),
                        "OR" | "REPLACE"
                            | "TEMPORARY"
                            | "UNIQUE"
                            | "FULLTEXT"
                            | "SPATIAL"
                            | "DEFINER"
                            | "ALGORITHM"
                            | "SQL"
                            | "SECURITY"
                            | "IF"
                            | "NOT"
                            | "EXISTS"
                            | "ONLINE"
                            | "OFFLINE"
                            | "UNDEFINED"
                            | "MERGE"
                    )
                })
                .cloned();
            match rest {
                Some(r) => format!("{first} {r}"),
                None => first.to_string(),
            }
        }
        "START" => "START TRANSACTION".into(),
        "TRUNCATE" => "TRUNCATE TABLE".into(),
        other => other.to_string(),
    })
}

/// Why a statement on a production connection needs typed confirmation, or `None`. A read-only
/// run is checked statement by statement anyway; what it cannot run is `Unknown` here.
pub fn prod_guard_reason(sql: &str, read_only: bool) -> Option<String> {
    let c = classify(sql);
    let needs = if read_only {
        c.overall == StatementClass::Unknown
    } else {
        c.overall >= StatementClass::Write
    };
    needs.then_some(c.summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro(s: &str) -> bool {
        check_read_only(s).is_ok()
    }

    #[test]
    fn plain_reads_pass() {
        for s in [
            "SELECT 1",
            "select * from orders where id = 5 order by created_at desc limit 10",
            "SELECT `update`, `insert` FROM `t`",
            "WITH x AS (SELECT 1 AS a) SELECT a FROM x",
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 5) SELECT * FROM c",
            "(SELECT 1) UNION (SELECT 2)",
            "TABLE orders",
            "VALUES ROW(1, 2), ROW(3, 4)",
            "SHOW TABLES",
            "SHOW FULL COLUMNS FROM orders",
            "SHOW CREATE TABLE orders",
            "SHOW PROCEDURE STATUS WHERE Db = 'shop'",
            "SHOW TABLE STATUS LIKE 'o%'",
            "DESCRIBE orders",
            "DESC orders",
            "EXPLAIN SELECT * FROM orders",
            "EXPLAIN ANALYZE SELECT * FROM orders",
            "EXPLAIN FORMAT=TREE SELECT 1",
            "SELECT CAST(x AS DECIMAL(10,2)), CONVERT(y, CHAR(5)) FROM t",
            "SELECT CAST(x AS CHAR CHARACTER SET utf8mb4) FROM t",
            "SELECT REPLACE(name, 'a', 'b'), INSERT(name, 1, 2, 'x'), TRUNCATE(1.5, 0) FROM t",
            "SELECT COUNT(*), MAX(a) OVER (PARTITION BY b ORDER BY c) FROM t GROUP BY b",
            "SELECT * FROM t WHERE a IN (1, 2) AND EXISTS (SELECT 1 FROM u)",
            "SELECT * FROM t USE INDEX (idx_a) WHERE a LIKE '%x%'",
            "SELECT * FROM JSON_TABLE('[1]', '$[*]' COLUMNS (id INT PATH '$', n FOR ORDINALITY)) j",
            "SELECT * FROM (SELECT 1, 2) AS d (a, b)",
            "SELECT @@version, @@global.max_connections, @x",
            "SELECT 'it''s', \"quoted\", _utf8mb4'x', X'41', b'01', N'n'",
            "SELECT 1 -- trailing comment",
            "SELECT 1 # hash comment",
            "SELECT /* plain comment */ 1",
            "SELECT SUBSTRING(s FROM 1 FOR 3) FROM t",
            "SELECT MATCH (a) AGAINST ('x' IN BOOLEAN MODE) FROM t",
            "SELECT 'a\\_b' LIKE 'a\\_%'",
        ] {
            assert!(ro(s), "{s}: {:?}", check_read_only(s));
        }
    }

    /// Each of these gets past a READ ONLY transaction on the server, or would make the text
    /// mean something else than it reads, so the check must refuse it.
    #[test]
    fn hostile_reads_are_refused() {
        for s in [
            // files on the server
            "SELECT * FROM t INTO OUTFILE '/tmp/x'",
            "SELECT * INTO DUMPFILE '/tmp/x' FROM t",
            "SELECT a INTO @v FROM t",
            "SELECT LOAD_FILE('/etc/passwd')",
            "SELECT load_file ('/etc/passwd')",
            "TABLE t INTO OUTFILE '/tmp/x'",
            // locks, sleeps, side effects
            "SELECT SLEEP(10)",
            "SELECT BENCHMARK(100000000, MD5('x'))",
            "SELECT GET_LOCK('x', 10)",
            "SELECT RELEASE_ALL_LOCKS()",
            "SELECT LAST_INSERT_ID(42)",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t FOR SHARE",
            "SELECT * FROM t LOCK IN SHARE MODE",
            "SELECT @a := 1",
            "SELECT * FROM t WHERE (@a := @a + 1) > 0",
            // stored and loadable functions
            "SELECT sys_exec('id')",
            "SELECT shop.do_things()",
            "SELECT `sleep`(5)",
            // Audit finding 2: under ANSI_QUOTES these are calls.
            "SELECT \"SLEEP\"(1000)",
            "SELECT \"LOAD_FILE\"('/etc/passwd')",
            "SELECT \"GET_LOCK\"('x', 100)",
            "SELECT \"shop\".\"fn\"()",
            "SELECT \"shop\" . \"fn\" ()",
            "SELECT my_udf(a) FROM t",
            // statements that are not reads
            "SET GLOBAL max_connections = 1",
            "SET PERSIST max_connections = 1",
            "SET @@global.read_only = 0",
            "SET SESSION TRANSACTION READ WRITE",
            "KILL 12",
            "KILL QUERY 12",
            "HANDLER t OPEN",
            "DO SLEEP(5)",
            "CALL p()",
            "LOAD DATA LOCAL INFILE '/etc/passwd' INTO TABLE t",
            "INSERT INTO t VALUES (1)",
            "REPLACE INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "WITH x AS (SELECT 1) UPDATE t SET a = 1",
            "WITH x AS (SELECT 1) DELETE FROM t",
            "COMMIT",
            "START TRANSACTION READ WRITE",
            "USE other",
            "DELIMITER //",
            "PREPARE s FROM 'DELETE FROM t'",
            "EXECUTE s",
            "EXPLAIN FOR CONNECTION 12",
            "SHOW CREATE USER root",
            "SHOW TABLES WHERE SLEEP(5)",
            // Audit finding 6.
            "SHOW TABLES WHERE EXISTS (SELECT 1 FROM orders FOR SHARE)",
            "SHOW TABLES WHERE EXISTS (SELECT 1 FROM orders LOCK IN SHARE MODE)",
            "SHOW TABLES WHERE EXISTS (SELECT 1 FROM orders)",
            "SHOW COLUMNS FROM t FOR UPDATE",
            "SELECT * FROM t PROCEDURE ANALYSE()",
            // lexing tricks
            "SELECT 1 /*!50000 ; DROP TABLE t */",
            "SELECT /*!50000 SLEEP(5), */ 1",
            "/*!50000 DROP TABLE t */",
            "SELECT /*+ SET_VAR(sql_mode='') */ 1",
            "SELECT 1; DROP TABLE t",
            "SELECT 'a\\'; DROP TABLE t; -- '",
            "SELECT \"a\\\"; DELETE FROM t; -- \"",
            "SELECT 1 #x\rDELETE FROM t",
            "SELECT 1 --x\nDELETE FROM t",
            "SELECT 1 FROM t WHERE a = 1\u{0} OR SLEEP(1)",
            "SELECT 1 /* unterminated",
            "SELECT 'unterminated",
            "SELECT \\N",
            "SELECT ＳＬＥＥＰ(1)",
            "SELECT 1 FROM tést",
        ] {
            assert!(!ro(s), "{s} passed the read-only check");
        }
    }

    #[test]
    fn refusals_say_why() {
        let e = check_read_only("SELECT SLEEP(10)").unwrap_err();
        assert!(e.contains("SLEEP()"), "{e}");
        let e = check_read_only("SELECT shop.f()").unwrap_err();
        assert!(e.contains("stored function"), "{e}");
        let e = check_read_only("SELECT /*!1 2*/").unwrap_err();
        assert!(e.contains("executable comment"), "{e}");
        let e = check_read_only("SELECT 'a\\'b'").unwrap_err();
        assert!(e.contains("NO_BACKSLASH_ESCAPES"), "{e}");
    }

    #[test]
    fn splitting_follows_the_server_lexer() {
        assert_eq!(
            split_script("SELECT 1; SELECT ';'; SELECT `a;b`; -- c;\nSELECT 2 # d;\n;"),
            vec![
                "SELECT 1",
                "SELECT ';'",
                "SELECT `a;b`",
                "-- c;\nSELECT 2 # d;"
            ]
        );
        // `\r` does not end a `#` comment: the `;` after it is inside the comment.
        assert_eq!(
            split_script("SELECT 1 #x\r; DROP TABLE t"),
            vec!["SELECT 1 #x\r; DROP TABLE t"]
        );
        // A backslash-escaped quote does not end the string.
        assert_eq!(
            split_script("SELECT 'a\\'; b'; SELECT 2"),
            vec!["SELECT 'a\\'; b'", "SELECT 2"]
        );
        // `--1` is not a comment.
        assert_eq!(
            split_script("SELECT 1--1; SELECT 2"),
            vec!["SELECT 1--1", "SELECT 2"]
        );
        assert!(split_script("  ;; ; ").is_empty());
    }

    #[test]
    fn routine_bodies_stay_whole() {
        let sql = "CREATE PROCEDURE p(IN x INT)\nBEGIN\n  DECLARE y INT DEFAULT 0;\n  IF x > 0 THEN SET y = 1; END IF;\n  SET y = CASE WHEN x > 1 THEN 2 ELSE 3 END;\n  lbl: LOOP LEAVE lbl; END LOOP;\n  SELECT y;\nEND;\nSELECT 1";
        let parts = split_script(sql);
        assert_eq!(parts.len(), 2, "{parts:#?}");
        assert!(parts[0].ends_with("END"));
        assert_eq!(parts[1], "SELECT 1");
        let trig = "CREATE DEFINER=`root`@`%` TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET NEW.a = 1; END; SELECT 2";
        assert_eq!(split_script(trig).len(), 2);
        // An ordinary BEGIN is a transaction, not a block.
        assert_eq!(split_script("BEGIN; SELECT 1; COMMIT").len(), 3);
    }

    #[test]
    fn delimiter_is_refused_with_advice() {
        let stmts = split_script("DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; END //");
        let e = check_no_delimiter(&stmts).unwrap_err();
        assert!(e.contains("mysql command-line client"), "{e}");
        assert!(check_no_delimiter(&split_script("SELECT 1")).is_ok());
    }

    #[test]
    fn classification() {
        let c = |s: &str| classify(s).overall;
        assert_eq!(c("SELECT 1"), StatementClass::ReadOnly);
        assert_eq!(c("SHOW DATABASES"), StatementClass::ReadOnly);
        assert_eq!(c("INSERT INTO t VALUES (1)"), StatementClass::Write);
        assert_eq!(
            c("WITH x AS (SELECT 1) UPDATE t SET a = 1"),
            StatementClass::Write
        );
        assert_eq!(c("SELECT * FROM t FOR UPDATE"), StatementClass::Write);
        assert_eq!(c("CREATE TABLE t (a INT)"), StatementClass::Ddl);
        assert_eq!(c("SET GLOBAL max_connections = 5"), StatementClass::Ddl);
        assert_eq!(c("SET @x = 1"), StatementClass::Session);
        assert_eq!(c("USE shop"), StatementClass::Session);
        assert_eq!(c("BEGIN"), StatementClass::Session);
        // Audit finding 4: these change the server or other sessions.
        for (sql, class) in [
            ("SET PASSWORD FOR 'app'@'%' = 'x'", StatementClass::Ddl),
            ("SET PASSWORD = 'x'", StatementClass::Ddl),
            ("SET DEFAULT ROLE ALL TO 'app'@'%'", StatementClass::Ddl),
            ("SET ROLE admin", StatementClass::Ddl),
            ("SET RESOURCE GROUP rg FOR 42", StatementClass::Ddl),
            ("START REPLICA", StatementClass::Ddl),
            ("START GROUP_REPLICATION", StatementClass::Ddl),
            ("XA COMMIT 'xid'", StatementClass::Write),
            ("XA ROLLBACK 'xid'", StatementClass::Write),
            ("LOCK TABLES t WRITE", StatementClass::Write),
            ("LOCK INSTANCE FOR BACKUP", StatementClass::Write),
            ("START TRANSACTION", StatementClass::Session),
            ("SET NAMES utf8mb4", StatementClass::Session),
            (
                "SET SESSION TRANSACTION ISOLATION LEVEL READ COMMITTED",
                StatementClass::Session,
            ),
            ("UNLOCK TABLES", StatementClass::Session),
        ] {
            assert_eq!(c(sql), class, "{sql}");
            if class != StatementClass::Session {
                assert!(
                    prod_guard_reason(sql, false).is_some(),
                    "{sql} skips the prod guard"
                );
            }
        }
        assert_eq!(c("SELECT SLEEP(1)"), StatementClass::Unknown);
        assert_eq!(c("SELECT 1 INTO OUTFILE '/tmp/x'"), StatementClass::Unknown);
        assert_eq!(c("CALL p()"), StatementClass::Unknown);
        assert_eq!(
            c("SELECT 1 /*!50000 , SLEEP(1) */"),
            StatementClass::Unknown
        );
        assert_eq!(c("SELECT 'open"), StatementClass::Unknown);
        let s = classify("SELECT 1; DROP TABLE t");
        assert_eq!(s.overall, StatementClass::Ddl);
        assert_eq!(s.summary, "2 statements: SELECT, DROP TABLE");
    }

    #[test]
    fn agent_refusals() {
        for (sql, needle) in [
            ("SELECT * FROM t INTO OUTFILE '/tmp/x'", "OUTFILE"),
            (
                "LOAD DATA INFILE '/var/lib/mysql-files/x' INTO TABLE t",
                "LOAD DATA",
            ),
            ("SELECT LOAD_FILE('/etc/passwd')", "LOAD_FILE"),
            ("SET GLOBAL general_log = 1", "SET GLOBAL"),
            ("SET @@persist.max_connections = 1", "SET GLOBAL"),
            ("KILL 5", "KILL"),
            ("INSTALL PLUGIN x SONAME 'x.so'", "native code"),
            (
                "CREATE FUNCTION sys_exec RETURNS INT SONAME 'lib_mysqludf_sys.so'",
                "native code",
            ),
            ("USE other", "USE"),
            ("SELECT 1 /*!50000 , 2 */", "executable comment"),
            ("DELIMITER $$", "DELIMITER"),
            // Audit finding 5.
            ("SELECT `LOAD_FILE`('/etc/passwd')", "LOAD_FILE"),
            ("SELECT \"load_file\"('/etc/passwd')", "LOAD_FILE"),
            (
                "SET @q = 0x53454c454354; PREPARE s FROM @q; EXECUTE s",
                "PREPARE",
            ),
            (
                "PREPARE s FROM 'SELECT 1 INTO OUTFILE ''/tmp/x'''",
                "PREPARE",
            ),
            ("EXECUTE s", "PREPARE"),
            ("DEALLOCATE PREPARE s", "PREPARE"),
            ("SELECT 'a\\' INTO OUTFILE '/tmp/x'", "one way only"),
            ("SELECT 'open", "one way only"),
        ] {
            let r = agent_refusal(sql).unwrap_or_else(|| panic!("{sql} was not refused"));
            assert!(r.contains(needle), "{sql}: {r}");
        }
        for sql in [
            "SELECT 1",
            "UPDATE t SET a = 1",
            "SET @x = 1",
            "CREATE TABLE t (a INT)",
            "SELECT 'INTO OUTFILE'",
        ] {
            assert_eq!(agent_refusal(sql), None, "{sql}");
        }
    }

    #[test]
    fn prod_guard() {
        assert_eq!(prod_guard_reason("SELECT 1", false), None);
        assert!(prod_guard_reason("DELETE FROM t", false).is_some());
        assert!(prod_guard_reason("SELECT SLEEP(1)", true).is_some());
        assert_eq!(prod_guard_reason("UPDATE t SET a = 1", true), None);
    }

    #[test]
    fn command_tags() {
        assert_eq!(
            command_tag("insert into t values (1)").as_deref(),
            Some("INSERT")
        );
        assert_eq!(
            command_tag("CREATE OR REPLACE VIEW v AS SELECT 1").as_deref(),
            Some("CREATE VIEW")
        );
        assert_eq!(
            command_tag("create temporary table if not exists x (a int)").as_deref(),
            Some("CREATE TABLE")
        );
        assert_eq!(
            command_tag("/* c */ DROP TABLE x").as_deref(),
            Some("DROP TABLE")
        );
    }
}
