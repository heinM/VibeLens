//! A small SQL tokenizer that can lex either way a backslash in a string literal may be read.
//!
//! Redshift treats `\'` inside an ordinary `'…'` literal as an escaped quote; standard-conforming
//! PostgreSQL (what `crate::sql` lexes and classifies) treats `'\'` as a complete string. The two
//! readings agree on everything up to the first backslash inside a string literal, so a statement
//! with no such backslash means the same thing to the classifier and to Redshift. [`lex`] is used
//! both to find that backslash under each reading and, once there is none, to scan the
//! statement's tokens (top-level `;`, function calls).
//!
//! Everything else follows `crate::sql`'s lexer exactly where it decides what is a string, a
//! quoted identifier or a comment: the `E'`/`B'`/`X'`/`N'`/`U&'`/`U&"` prefixes, nested block
//! comments, `--` comments ending at `\n` or `\r`, and the dollar-quote tag rule (a tag starts with
//! a letter, `_` or any byte >= 0x80). [`Lexed::spans`] records those spans so the two lexers can
//! be compared (`check::check_unambiguous` and its tests).

use crate::sql::SpanKind;

/// One token. Comments and whitespace are dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tok {
    /// A bare word (keyword or identifier), lower-cased.
    Word(String),
    /// A `"quoted identifier"`, unquoted (`""` folded to `"`), case kept: with case-sensitive
    /// identifiers on, `"LOWER"` is not `lower`.
    QuotedIdent(String),
    /// A string literal (`'…'` or a `$tag$…$tag$` body). The flag is set when a backslash occurs
    /// inside a `'…'` literal.
    Str { backslash: bool },
    /// A number or a positional parameter (`$1`).
    Number,
    /// Any other single character outside literals and comments (`(`, `.`, `;`, `,`, …).
    Punct(char),
}

#[derive(Debug, Default)]
pub(super) struct Lexed {
    pub toks: Vec<Tok>,
    /// String literals, quoted identifiers and comments as byte ranges, in order.
    pub spans: Vec<(SpanKind, usize, usize)>,
    /// A quote, dollar quote or block comment was left open.
    pub unbalanced: bool,
    pub has_comment: bool,
}

impl Lexed {
    /// Whether any `'…'` literal contains a backslash.
    pub fn backslash_in_string(&self) -> bool {
        self.toks
            .iter()
            .any(|t| matches!(t, Tok::Str { backslash: true }))
    }

    /// Words only, upper-cased: the shape `check` reasons about.
    pub fn words_upper(&self) -> Vec<String> {
        self.toks
            .iter()
            .filter_map(|t| match t {
                Tok::Word(w) => Some(w.to_ascii_uppercase()),
                _ => None,
            })
            .collect()
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// A `'…'` body from just after the opening quote: (end, saw a backslash, closed).
fn single_quoted(b: &[u8], mut i: usize, backslash_escapes: bool) -> (usize, bool, bool) {
    let mut backslash = false;
    while i < b.len() {
        match b[i] {
            b'\'' if b.get(i + 1) == Some(&b'\'') => i += 2,
            b'\'' => return (i + 1, backslash, true),
            b'\\' => {
                backslash = true;
                i += if backslash_escapes { 2 } else { 1 };
            }
            _ => i += 1,
        }
    }
    (b.len(), backslash, false)
}

/// A `"…"` body from just after the opening quote: (end, text, closed).
fn double_quoted(b: &[u8], mut i: usize) -> (usize, String, bool) {
    let mut text = Vec::new();
    while i < b.len() {
        match b[i] {
            b'"' if b.get(i + 1) == Some(&b'"') => {
                text.push(b'"');
                i += 2;
            }
            b'"' => return (i + 1, String::from_utf8_lossy(&text).into_owned(), true),
            x => {
                text.push(x);
                i += 1;
            }
        }
    }
    (b.len(), String::from_utf8_lossy(&text).into_owned(), false)
}

/// Lexes `sql`. With `backslash_escapes`, `\` inside `'…'` escapes the next character
/// (Redshift); without it, `\` is an ordinary character (standard-conforming strings).
pub(super) fn lex(sql: &str, backslash_escapes: bool) -> Lexed {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Lexed::default();
    let mut i = 0;
    while i < n {
        let c = b[i];
        let start = i;
        match c {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                out.has_comment = true;
                while i < n && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
                out.spans.push((SpanKind::Comment, start, i));
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.has_comment = true;
                // Block comments nest in PostgreSQL and Redshift.
                let mut depth = 1;
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
                i = i.min(n);
                if depth > 0 {
                    out.unbalanced = true;
                }
                out.spans.push((SpanKind::Comment, start, i));
            }
            b'\'' => {
                let (end, backslash, closed) = single_quoted(b, i + 1, backslash_escapes);
                i = end;
                out.unbalanced |= !closed;
                out.toks.push(Tok::Str { backslash });
                out.spans.push((SpanKind::Str, start, i));
            }
            b'"' => {
                let (end, text, closed) = double_quoted(b, i + 1);
                i = end;
                out.unbalanced |= !closed;
                out.toks.push(Tok::QuotedIdent(text));
                out.spans.push((SpanKind::QuotedIdent, start, i));
            }
            b'$' => {
                // `$1`: a parameter. `$tag$ … $tag$`: a literal body. The tag rule is
                // `crate::sql`'s: empty, or an identifier start then identifier characters
                // (bytes >= 0x80 included) up to the next `$`.
                if b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    while i < n && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    out.toks.push(Tok::Number);
                    continue;
                }
                let mut j = i + 1;
                if j < n && is_ident_start(b[j]) {
                    while j < n && is_ident_cont(b[j]) && b[j] != b'$' {
                        j += 1;
                    }
                }
                if j < n && b[j] == b'$' {
                    let delim = &b[i..=j];
                    let body = j + 1;
                    match b[body..].windows(delim.len()).position(|w| w == delim) {
                        Some(p) => i = body + p + delim.len(),
                        None => {
                            out.unbalanced = true;
                            i = n;
                        }
                    }
                    out.toks.push(Tok::Str { backslash: false });
                    out.spans.push((SpanKind::Str, start, i));
                } else {
                    out.toks.push(Tok::Punct('$'));
                    i += 1;
                }
            }
            _ if c.is_ascii_digit() => {
                while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                    i += 1;
                }
                out.toks.push(Tok::Number);
            }
            _ if is_ident_start(c) => {
                while i < n && is_ident_cont(b[i]) {
                    i += 1;
                }
                let len = i - start;
                let next = b.get(i).copied();
                // String prefixes, as `crate::sql` reads them: `E'…'`, `B'…'`, `X'…'`, `N'…'`,
                // `U&'…'` and `U&"…"`. The prefix belongs to the literal's span.
                if len == 1 && next == Some(b'\'') && b"eEbBxXnN".contains(&c) {
                    let (end, backslash, closed) = single_quoted(b, i + 1, backslash_escapes);
                    i = end;
                    out.unbalanced |= !closed;
                    out.toks.push(Tok::Str { backslash });
                    out.spans.push((SpanKind::Str, start, i));
                    continue;
                }
                if len == 1 && (c == b'u' || c == b'U') && next == Some(b'&') {
                    match b.get(i + 1) {
                        Some(b'\'') => {
                            let (end, backslash, closed) =
                                single_quoted(b, i + 2, backslash_escapes);
                            i = end;
                            out.unbalanced |= !closed;
                            out.toks.push(Tok::Str { backslash });
                            out.spans.push((SpanKind::Str, start, i));
                            continue;
                        }
                        Some(b'"') => {
                            let (end, text, closed) = double_quoted(b, i + 2);
                            i = end;
                            out.unbalanced |= !closed;
                            out.toks.push(Tok::QuotedIdent(text));
                            out.spans.push((SpanKind::QuotedIdent, start, i));
                            continue;
                        }
                        _ => {}
                    }
                }
                out.toks.push(Tok::Word(
                    String::from_utf8_lossy(&b[start..i]).to_lowercase(),
                ));
            }
            _ if c.is_ascii_whitespace() || c == 0x0b => i += 1,
            _ => {
                // Bytes >= 0x80 start words above, so this is ASCII punctuation.
                out.toks.push(Tok::Punct(c as char));
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_readings_differ_only_after_a_backslash_in_a_literal() {
        let sql = r"SELECT '\' , ' AS x INTO public.pwned FROM secrets -- '";
        let std = lex(sql, false);
        let rs = lex(sql, true);
        assert!(std.backslash_in_string() && rs.backslash_in_string());
        // Standard: two literals then a comment. Redshift: one literal, then INTO … FROM.
        assert!(!std.toks.contains(&Tok::Word("into".into())));
        assert!(rs.toks.contains(&Tok::Word("into".into())));

        let plain = "SELECT 'it''s', \"a;b\" FROM t -- x\n";
        assert_eq!(lex(plain, false).toks, lex(plain, true).toks);
        assert!(!lex(plain, true).backslash_in_string());
    }

    #[test]
    fn dollar_quotes_parameters_and_identifiers() {
        let l = lex(
            "CREATE PROCEDURE p() AS $$ BEGIN x := 'a;b'; END; $$ LANGUAGE plpgsql",
            true,
        );
        assert!(!l.toks.contains(&Tok::Punct(';')));
        assert!(!l.unbalanced);
        let l = lex("SELECT $1, sales$2024 FROM \"My\"\"T\"", true);
        assert!(l.toks.contains(&Tok::Word("sales$2024".into())));
        assert!(l.toks.contains(&Tok::QuotedIdent("My\"T".into())));
        assert!(lex("SELECT 'open", true).unbalanced);
        assert!(lex("SELECT $x$ open", true).unbalanced);
        // A tag may hold non-ASCII bytes, as in `crate::sql`.
        let l = lex("SELECT $é$ ' $é$ , 1", true);
        assert_eq!(l.spans.len(), 1);
        assert!(!l.unbalanced);
    }
}
