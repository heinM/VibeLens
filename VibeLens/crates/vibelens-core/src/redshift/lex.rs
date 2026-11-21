//! A small SQL tokenizer that can lex either way a backslash in a string literal may be read.
//!
//! Redshift treats `\'` inside an ordinary `'…'` literal as an escaped quote; standard-conforming
//! PostgreSQL (what `crate::sql` lexes and classifies) treats `'\'` as a complete string. The two
//! readings agree on everything up to the first backslash inside a string literal, so a statement
//! with no such backslash means the same thing to the classifier and to Redshift. [`lex`] is used
//! both to find that backslash under each reading and, once there is none, to scan the
//! statement's tokens (top-level `;`, function calls).

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

/// Lexes `sql`. With `backslash_escapes`, `\` inside `'…'` escapes the next character
/// (Redshift); without it, `\` is an ordinary character (standard-conforming strings).
pub(super) fn lex(sql: &str, backslash_escapes: bool) -> Lexed {
    let b = sql.as_bytes();
    let mut out = Lexed::default();
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
                        out.unbalanced = true;
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
            b'\'' => {
                let mut backslash = false;
                i += 1;
                loop {
                    match b.get(i) {
                        None => {
                            out.unbalanced = true;
                            break;
                        }
                        Some(b'\'') if b.get(i + 1) == Some(&b'\'') => i += 2,
                        Some(b'\'') => {
                            i += 1;
                            break;
                        }
                        Some(b'\\') => {
                            backslash = true;
                            i += if backslash_escapes { 2 } else { 1 };
                        }
                        Some(_) => i += 1,
                    }
                }
                out.toks.push(Tok::Str { backslash });
            }
            b'"' => {
                let mut text = Vec::new();
                i += 1;
                loop {
                    match b.get(i) {
                        None => {
                            out.unbalanced = true;
                            break;
                        }
                        Some(b'"') if b.get(i + 1) == Some(&b'"') => {
                            text.push(b'"');
                            i += 2;
                        }
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(&x) => {
                            text.push(x);
                            i += 1;
                        }
                    }
                }
                out.toks.push(Tok::QuotedIdent(
                    String::from_utf8_lossy(&text).into_owned(),
                ));
            }
            b'$' => {
                // `$1`: a parameter. `$tag$ … $tag$` (tag empty or an identifier): a literal body.
                if b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    out.toks.push(Tok::Number);
                    continue;
                }
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if b.get(j) == Some(&b'$') && !b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    let delim = &b[i..=j];
                    let body = j + 1;
                    match b[body..].windows(delim.len()).position(|w| w == delim) {
                        Some(p) => i = body + p + delim.len(),
                        None => {
                            out.unbalanced = true;
                            i = b.len();
                        }
                    }
                    out.toks.push(Tok::Str { backslash: false });
                } else {
                    out.toks.push(Tok::Punct('$'));
                    i += 1;
                }
            }
            _ if c.is_ascii_digit() => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                out.toks.push(Tok::Number);
            }
            _ if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                let start = i;
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'_'
                        || b[i] == b'$'
                        || b[i] >= 0x80)
                {
                    i += 1;
                }
                out.toks.push(Tok::Word(
                    String::from_utf8_lossy(&b[start..i]).to_lowercase(),
                ));
            }
            _ if c.is_ascii_whitespace() => i += 1,
            _ => {
                // Multi-byte characters outside words are consumed whole above (>= 0x80), so
                // this is ASCII punctuation.
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
    }
}
