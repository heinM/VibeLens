//! A lexer that follows the MySQL 8 server's own (`sql_lex.cc`) where it matters for deciding
//! what a statement is: where strings, identifiers and comments start and end.
//!
//! - `'…'` and `"…"` are strings (or, under `ANSI_QUOTES`, `"…"` is an identifier); a doubled
//!   quote is one quote, and `\` escapes the next byte unless `NO_BACKSLASH_ESCAPES` is set.
//!   Which of those applies depends on the session's `sql_mode`, which the client cannot know
//!   for sure, so the lexer takes it as a parameter and callers that must be certain lex both
//!   ways ([`lex_unambiguous`]).
//! - `` `…` `` is an identifier; a doubled backtick is one backtick; no escapes.
//! - `#` starts a comment to the next `\n`. So does `--`, but only when followed by whitespace,
//!   a control character or the end of the text (`--1` is "minus minus one"). `\r` does not end
//!   a line comment.
//! - `/* … */` comments do not nest. `/*! … */` (executable comment: the server runs what is
//!   inside) and `/*+ … */` (optimizer hint) are their own token kinds, because the server does
//!   not ignore them. An unterminated comment, string or identifier is an error.

/// What a token is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Keyword or bare identifier (also numbers: `1`, `0x1F`, `1e5`; MySQL lets identifiers
    /// start with a digit).
    Word,
    /// `` `quoted identifier` ``.
    Backtick,
    /// `'…'` or `"…"`.
    Str,
    /// `@name`, `@@name`, `@'quoted'`, `@@global.name`.
    Variable,
    /// `:=`.
    Assign,
    /// Any other single byte outside quotes and comments (`(`, `,`, `;`, `.`, `\`, …).
    Punct,
    /// `#` or `-- ` comment.
    LineComment,
    /// `/* … */`.
    BlockComment,
    /// `/*! … */`: run by the server.
    ExecComment,
    /// `/*+ … */`: optimizer hint.
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tok {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

/// Why a text could not be lexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LexError {
    Unterminated(&'static str),
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::Unterminated(what) => write!(f, "unterminated {what}"),
        }
    }
}

/// Whitespace as the server's `my_isspace` sees it in ASCII.
pub(crate) fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

/// Identifier bytes: ASCII letters, digits, `_`, `$`, and every byte of a multi-byte character.
pub(crate) fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// `--` starts a comment only before whitespace, a control character or the end of the text.
fn dash_dash_comment(b: &[u8], i: usize) -> bool {
    b[i] == b'-'
        && b.get(i + 1) == Some(&b'-')
        && b.get(i + 2).is_none_or(|&c| c <= b' ' || c == 0x7F)
}

/// Index just past a quoted run that starts at `i` with `quote`.
fn scan_quoted(b: &[u8], i: usize, quote: u8, backslash: bool) -> Result<usize, LexError> {
    let mut j = i + 1;
    while j < b.len() {
        let c = b[j];
        if backslash && c == b'\\' {
            j += 2;
        } else if c == quote {
            if b.get(j + 1) == Some(&quote) {
                j += 2;
            } else {
                return Ok(j + 1);
            }
        } else {
            j += 1;
        }
    }
    Err(LexError::Unterminated(match quote {
        b'`' => "quoted identifier",
        _ => "string",
    }))
}

/// Tokens of `sql`. `backslash`: `\` escapes inside `'…'` and `"…"` (the server default; off
/// under `NO_BACKSLASH_ESCAPES`). Whitespace is skipped.
pub(crate) fn lex(sql: &str, backslash: bool) -> Result<Vec<Tok>, LexError> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        let kind;
        if is_space(c) {
            i += 1;
            continue;
        } else if c == b'#' || dash_dash_comment(b, i) {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            kind = Kind::LineComment;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            kind = match b.get(i + 2) {
                Some(b'!') => Kind::ExecComment,
                // MariaDB (`/*M! … */`) and TiDB (`/*T! … */`) executable comments: MySQL reads
                // them as plain comments, but a compatible server or proxy runs what is inside.
                Some(b'M' | b'T') if b.get(i + 3) == Some(&b'!') => Kind::ExecComment,
                Some(b'+') => Kind::Hint,
                _ => Kind::BlockComment,
            };
            // `/*/` does not close the comment it opens: the search starts after `/*`.
            let close = b[i + 2..]
                .windows(2)
                .position(|w| w == b"*/")
                .ok_or(LexError::Unterminated("comment"))?;
            i = i + 2 + close + 2;
        } else if c == b'\'' || c == b'"' {
            i = scan_quoted(b, i, c, backslash)?;
            kind = Kind::Str;
        } else if c == b'`' {
            i = scan_quoted(b, i, c, false)?;
            kind = Kind::Backtick;
        } else if c == b'@' {
            i += 1;
            if b.get(i) == Some(&b'@') {
                i += 1;
            }
            match b.get(i) {
                Some(&q @ (b'\'' | b'"')) => i = scan_quoted(b, i, q, backslash)?,
                Some(b'`') => i = scan_quoted(b, i, b'`', false)?,
                _ => {
                    while i < b.len() && (is_ident(b[i]) || b[i] == b'.') {
                        i += 1;
                    }
                }
            }
            kind = Kind::Variable;
        } else if c == b':' && b.get(i + 1) == Some(&b'=') {
            i += 2;
            kind = Kind::Assign;
        } else if is_ident(c) {
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            kind = Kind::Word;
        } else {
            i += 1;
            kind = Kind::Punct;
        }
        out.push(Tok {
            kind,
            start,
            end: i.min(b.len()),
        });
    }
    Ok(out)
}

/// Tokens that mean the same whether or not `\` escapes inside quotes (that is, whatever the
/// session's `NO_BACKSLASH_ESCAPES` and `ANSI_QUOTES` say), or an error saying why not.
pub(crate) fn lex_unambiguous(sql: &str) -> Result<Vec<Tok>, String> {
    let (with, without) = match (lex(sql, true), lex(sql, false)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), Err(_)) => return Err(e.to_string()),
        // One reading lexes and the other does not: that is the ambiguity below.
        (a, b) => (a.unwrap_or_default(), b.unwrap_or_default()),
    };
    if with != without {
        return Err(
            "a backslash in a quoted string changes where the string ends depending on the \
             server's sql_mode (NO_BACKSLASH_ESCAPES), so what runs cannot be known in advance. \
             Write the literal without a backslash (for example with CHAR() or a doubled quote)"
                .into(),
        );
    }
    Ok(with)
}

/// `t`'s text.
pub(crate) fn text<'a>(sql: &'a str, t: &Tok) -> &'a str {
    &sql[t.start..t.end]
}

/// `t`'s text upper-cased, for keyword comparisons (only meaningful for [`Kind::Word`]).
pub(crate) fn upper(sql: &str, t: &Tok) -> String {
    text(sql, t).to_ascii_uppercase()
}

/// Whether the token is a comment of any kind.
pub(crate) fn is_comment(t: &Tok) -> bool {
    matches!(
        t.kind,
        Kind::LineComment | Kind::BlockComment | Kind::ExecComment | Kind::Hint
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str, backslash: bool) -> Vec<(Kind, &str)> {
        lex(sql, backslash)
            .unwrap()
            .into_iter()
            .map(|t| (t.kind, text(sql, &t)))
            .collect()
    }

    #[test]
    fn comments_follow_the_server() {
        assert_eq!(
            kinds("a # x\nb", true),
            vec![
                (Kind::Word, "a"),
                (Kind::LineComment, "# x"),
                (Kind::Word, "b")
            ]
        );
        // `--` needs whitespace or a control character after it.
        assert_eq!(
            kinds("1--1", true),
            vec![
                (Kind::Word, "1"),
                (Kind::Punct, "-"),
                (Kind::Punct, "-"),
                (Kind::Word, "1")
            ]
        );
        assert_eq!(kinds("a -- x\nb", true)[1], (Kind::LineComment, "-- x"));
        assert_eq!(kinds("a --\tx\nb", true)[1], (Kind::LineComment, "--\tx"));
        assert_eq!(kinds("a --", true)[1], (Kind::LineComment, "--"));
        // `\r` does not end a line comment on the server: everything up to `\n` is comment.
        assert_eq!(
            kinds("a #x\rDROP\nb", true),
            vec![
                (Kind::Word, "a"),
                (Kind::LineComment, "#x\rDROP"),
                (Kind::Word, "b")
            ]
        );
        // Block comments do not nest; executable comments and hints are their own kinds.
        assert_eq!(
            kinds("/* a /* b */ c */", true),
            vec![
                (Kind::BlockComment, "/* a /* b */"),
                (Kind::Word, "c"),
                (Kind::Punct, "*"),
                (Kind::Punct, "/")
            ]
        );
        assert_eq!(
            kinds("/*!50000 DROP */", true),
            vec![(Kind::ExecComment, "/*!50000 DROP */")]
        );
        assert_eq!(
            kinds("SELECT /*+ SET_VAR(x=1) */ 1", true)[1],
            (Kind::Hint, "/*+ SET_VAR(x=1) */")
        );
        // Audit finding 9.
        assert_eq!(
            kinds("/*M!100100 DROP */", true),
            vec![(Kind::ExecComment, "/*M!100100 DROP */")]
        );
        assert_eq!(kinds("/*T! x */", true)[0].0, Kind::ExecComment);
        assert_eq!(kinds("/*Mx */", true)[0].0, Kind::BlockComment);
        assert_eq!(
            kinds("/*/ x */", true),
            vec![(Kind::BlockComment, "/*/ x */")]
        );
        assert!(lex("/* open", true).is_err());
    }

    #[test]
    fn quotes_and_backslashes() {
        assert_eq!(kinds(r"'a\'b'", true), vec![(Kind::Str, r"'a\'b'")]);
        assert!(
            lex(r"'a\'b'", false).is_err(),
            "the trailing quote opens a string"
        );
        assert_eq!(kinds("'it''s'", true), vec![(Kind::Str, "'it''s'")]);
        assert_eq!(kinds("\"a\"\"b\"", false), vec![(Kind::Str, "\"a\"\"b\"")]);
        // Backticks never take backslash escapes.
        assert_eq!(
            kinds(r"`a\` x", true),
            vec![(Kind::Backtick, r"`a\`"), (Kind::Word, "x")]
        );
        assert_eq!(kinds("`a``b`", true), vec![(Kind::Backtick, "`a``b`")]);
        // A comment marker inside quotes is text.
        assert_eq!(kinds("'#x' -- y", true)[0], (Kind::Str, "'#x'"));
        assert!(lex("'open", true).is_err());
        assert!(lex("`open", true).is_err());
    }

    #[test]
    fn variables_and_assignment() {
        assert_eq!(
            kinds("@a := @@global.max_connections", true),
            vec![
                (Kind::Variable, "@a"),
                (Kind::Assign, ":="),
                (Kind::Variable, "@@global.max_connections")
            ]
        );
        assert_eq!(kinds("@'x y'", true), vec![(Kind::Variable, "@'x y'")]);
    }

    #[test]
    fn ambiguity_is_detected() {
        assert!(lex_unambiguous(r"SELECT 'a\'b'").is_err());
        assert!(lex_unambiguous(r"SELECT 'a\'', 1").is_err());
        assert!(lex_unambiguous(r"SELECT 'a\' OR '1'='1'").is_err());
        // A backslash that does not move the end of the string is fine either way.
        assert!(lex_unambiguous(r"SELECT 'a\_b'").is_ok());
        assert!(lex_unambiguous("SELECT 'plain'").is_ok());
    }
}
