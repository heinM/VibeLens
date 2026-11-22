//! Tiny line-based unified diff used for `write_file` approval previews, and the rule for
//! showing characters that would otherwise not be seen ([`visible`]).
//!
//! The preview is what the user approves, so it has to be honest about itself: a diff that is
//! cut says how much is missing ([`DiffPreview`]), and no character is left to the renderer
//! that could hide, reorder or overwrite the text around it.

use std::borrow::Cow;

const CONTEXT: usize = 3;
/// Above this many cells the LCS table is skipped and the changed middle is shown as a replace.
const MAX_LCS_CELLS: usize = 4_000_000;
/// Hard cap on preview size (lines) so a rewrite of a huge file stays reviewable.
const MAX_PREVIEW_LINES: usize = 600;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Equal,
    Delete,
    Insert,
}

struct Op<'a> {
    kind: Kind,
    text: &'a str,
}

fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    s.strip_suffix('\n').unwrap_or(s).split('\n').collect()
}

fn diff_ops<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Op<'a>> {
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];

    let mut ops: Vec<Op<'a>> = old[..prefix]
        .iter()
        .map(|t| Op {
            kind: Kind::Equal,
            text: t,
        })
        .collect();
    if a.is_empty() || b.is_empty() || a.len().saturating_mul(b.len()) > MAX_LCS_CELLS {
        ops.extend(a.iter().map(|t| Op {
            kind: Kind::Delete,
            text: t,
        }));
        ops.extend(b.iter().map(|t| Op {
            kind: Kind::Insert,
            text: t,
        }));
    } else {
        // lcs[i][j] = LCS length of a[i..], b[j..]
        let (n, m) = (a.len(), b.len());
        let mut lcs = vec![0u32; (n + 1) * (m + 1)];
        let at = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[at(i, j)] = if a[i] == b[j] {
                    lcs[at(i + 1, j + 1)] + 1
                } else {
                    lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if a[i] == b[j] {
                ops.push(Op {
                    kind: Kind::Equal,
                    text: a[i],
                });
                i += 1;
                j += 1;
            } else if lcs[at(i + 1, j)] >= lcs[at(i, j + 1)] {
                ops.push(Op {
                    kind: Kind::Delete,
                    text: a[i],
                });
                i += 1;
            } else {
                ops.push(Op {
                    kind: Kind::Insert,
                    text: b[j],
                });
                j += 1;
            }
        }
        ops.extend(a[i..].iter().map(|t| Op {
            kind: Kind::Delete,
            text: t,
        }));
        ops.extend(b[j..].iter().map(|t| Op {
            kind: Kind::Insert,
            text: t,
        }));
    }
    ops.extend(old[old.len() - suffix..].iter().map(|t| Op {
        kind: Kind::Equal,
        text: t,
    }));
    ops
}

/// A diff as shown for approval, with what the display left out.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct DiffPreview {
    /// The diff, cut after [`MAX_PREVIEW_LINES`] lines, with hidden characters made visible.
    pub text: String,
    /// Lines of the diff (added, removed and context) that are not in `text` because of the cut.
    pub hidden_lines: usize,
    /// How many of the hidden lines add a line, and how many remove one.
    pub hidden_added: usize,
    pub hidden_removed: usize,
}

/// A character that is not seen as itself where text is displayed: it draws nothing, or it
/// changes how the text around it is laid out.
///
/// - Control characters (C0, DEL, C1) except tab. A line feed is a line end; so is a carriage
///   return in front of one, which [`visible`] leaves alone.
/// - Direction controls (U+061C, U+200E/F, U+202A–E, U+2066–9): they reorder what is displayed,
///   so a statement can read differently from what it is.
/// - Invisible characters: zero-width space and joiners, word joiner and the other U+2060–F
///   format characters, soft hyphen, byte-order mark, Mongolian vowel separator, the Hangul
///   fillers, interlinear annotation marks, and the "tag" block (U+E0000–7F), which can carry
///   text nobody sees.
/// - Line and paragraph separators (U+2028/9): a line break that is not a line feed.
fn is_hidden(c: char) -> bool {
    match c {
        '\t' => false,
        c if c.is_control() => true,
        '\u{00AD}'
        | '\u{061C}'
        | '\u{115F}'
        | '\u{1160}'
        | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}'
        | '\u{3164}'
        | '\u{FEFF}'
        | '\u{FFA0}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{E0000}'..='\u{E007F}' => true,
        _ => false,
    }
}

/// The hidden characters of `text` with their byte offsets. A line feed, and a carriage return
/// that ends a line (before a line feed, or last in the text), are line ends and not reported.
fn hidden_chars(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let bytes = text.as_bytes();
    text.char_indices().filter(move |&(i, c)| match c {
        '\n' => false,
        '\r' => !matches!(bytes.get(i + 1), None | Some(b'\n')),
        c => is_hidden(c),
    })
}

/// How many characters of `text` [`visible`] would replace.
pub(super) fn count_hidden_chars(text: &str) -> usize {
    hidden_chars(text).count()
}

/// `text` with every hidden character (see [`is_hidden`]) written as its code point, such as
/// `<U+202E>`, so that what is on screen is what is in the text.
pub(super) fn visible(text: &str) -> Cow<'_, str> {
    let mut found = hidden_chars(text).peekable();
    if found.peek().is_none() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut from = 0;
    for (i, c) in found {
        out.push_str(&text[from..i]);
        out.push_str(&format!("<U+{:04X}>", c as u32));
        from = i + c.len_utf8();
    }
    out.push_str(&text[from..]);
    Cow::Owned(out)
}

/// `n` with thousands separators, for sentences a person reads.
pub(super) fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

/// Unified diff of `old` (None = the file does not exist yet) against `new`.
pub(super) fn unified_diff(path: &str, old: Option<&str>, new: &str) -> DiffPreview {
    let path = visible(path);
    let old_lines = split_lines(old.unwrap_or(""));
    let new_lines = split_lines(new);
    if old.is_some() && old_lines == new_lines {
        return DiffPreview {
            text: format!("No changes: {path} already has this content."),
            ..Default::default()
        };
    }
    let ops = diff_ops(&old_lines, &new_lines);

    // Line numbers (0-based count of lines consumed) before each op.
    let mut a_idx = Vec::with_capacity(ops.len());
    let mut b_idx = Vec::with_capacity(ops.len());
    let (mut a, mut b) = (0usize, 0usize);
    for op in &ops {
        a_idx.push(a);
        b_idx.push(b);
        match op.kind {
            Kind::Equal => {
                a += 1;
                b += 1;
            }
            Kind::Delete => a += 1,
            Kind::Insert => b += 1,
        }
    }

    // Merge change windows (change ± context) into hunks.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if op.kind == Kind::Equal {
            continue;
        }
        let s = i.saturating_sub(CONTEXT);
        let e = (i + CONTEXT).min(ops.len() - 1);
        match ranges.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => ranges.push((s, e)),
        }
    }

    let mut out = String::new();
    if old.is_some() {
        out.push_str(&format!("--- a/{path}\n"));
    } else {
        out.push_str("--- /dev/null\n");
    }
    out.push_str(&format!("+++ b/{path}\n"));
    let mut emitted = 0usize;
    let mut hidden = DiffPreview::default();
    for (s, e) in ranges {
        let slice = &ops[s..=e];
        let old_len = slice.iter().filter(|o| o.kind != Kind::Insert).count();
        let new_len = slice.iter().filter(|o| o.kind != Kind::Delete).count();
        let old_start = if old_len == 0 { a_idx[s] } else { a_idx[s] + 1 };
        let new_start = if new_len == 0 { b_idx[s] } else { b_idx[s] + 1 };
        // Past the limit nothing more is shown, but everything is still counted.
        if emitted < MAX_PREVIEW_LINES {
            out.push_str(&format!(
                "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
            ));
        }
        for op in slice {
            if emitted >= MAX_PREVIEW_LINES {
                hidden.hidden_lines += 1;
                match op.kind {
                    Kind::Equal => {}
                    Kind::Delete => hidden.hidden_removed += 1,
                    Kind::Insert => hidden.hidden_added += 1,
                }
                continue;
            }
            let sign = match op.kind {
                Kind::Equal => ' ',
                Kind::Delete => '-',
                Kind::Insert => '+',
            };
            out.push(sign);
            out.push_str(&visible(op.text));
            out.push('\n');
            emitted += 1;
        }
    }
    if hidden.hidden_lines > 0 {
        out.push_str(&format!(
            "… diff truncated for display: {} more {} not shown ({} added, {} removed) …\n",
            grouped(hidden.hidden_lines),
            if hidden.hidden_lines == 1 {
                "line is"
            } else {
                "lines are"
            },
            grouped(hidden.hidden_added),
            grouped(hidden.hidden_removed),
        ));
    }
    hidden.text = out;
    hidden
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_file() {
        let d = unified_diff("a.sql", None, "select 1;\nselect 2;\n").text;
        assert_eq!(
            d,
            "--- /dev/null\n+++ b/a.sql\n@@ -0,0 +1,2 @@\n+select 1;\n+select 2;\n"
        );
    }

    #[test]
    fn unchanged() {
        assert!(
            unified_diff("a.sql", Some("x\n"), "x\n")
                .text
                .starts_with("No changes")
        );
        // Trailing-newline-only differences are not shown as changes.
        assert!(
            unified_diff("a.sql", Some("x"), "x\n")
                .text
                .starts_with("No changes")
        );
    }

    #[test]
    fn single_line_change_with_context() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let new = "1\n2\n3\n4\nFIVE\n6\n7\n8\n9\n10\n";
        let d = unified_diff("f.sql", Some(old), new).text;
        assert_eq!(
            d,
            "--- a/f.sql\n+++ b/f.sql\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+FIVE\n 6\n 7\n 8\n"
        );
    }

    #[test]
    fn two_distant_hunks() {
        let old: Vec<String> = (1..=30).map(|i| i.to_string()).collect();
        let mut new = old.clone();
        new[1] = "two".into();
        new[27] = "twenty-eight".into();
        let d = unified_diff("f", Some(&old.join("\n")), &new.join("\n")).text;
        assert_eq!(d.matches("@@ -").count(), 2, "{d}");
        assert!(d.contains("-2\n+two\n"));
        assert!(d.contains("-28\n+twenty-eight\n"));
    }

    #[test]
    fn insert_and_delete() {
        let d = unified_diff("f", Some("a\nb\nc\n"), "a\nc\nd\n").text;
        assert!(d.contains("-b\n"));
        assert!(d.contains("+d\n"));
        assert!(d.contains(" a\n"));
    }

    #[test]
    fn huge_rewrite_is_capped() {
        let old: String = (0..5000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..5000).map(|i| format!("new {i}\n")).collect();
        let d = unified_diff("big", Some(&old), &new).text;
        assert!(d.contains("diff truncated"));
        assert!(d.lines().count() < 700);
    }

    /// 5000 lines replaced by 5000 others: one hunk of 5000 removals then 5000 additions, of
    /// which the first 600 lines are shown.
    #[test]
    fn a_truncated_diff_says_how_much_of_it_is_not_shown() {
        let old: String = (0..5000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..5000).map(|i| format!("new {i}\n")).collect();
        let d = unified_diff("big", Some(&old), &new);
        assert_eq!(
            (d.hidden_lines, d.hidden_removed, d.hidden_added),
            (9400, 4400, 5000)
        );
        let last = d.text.lines().last().unwrap();
        assert!(
            last.contains("diff truncated")
                && last.contains("9,400")
                && last.contains("5,000 added")
                && last.contains("4,400 removed"),
            "{last}"
        );
        // Lines of a later hunk count too.
        let mut far = old.clone();
        far.push_str("tail\n");
        let d = unified_diff("big", Some(&far), &format!("{new}TAIL\n"));
        assert_eq!(d.hidden_lines, 9402, "{:?}", d.text.lines().last());
    }

    #[test]
    fn a_diff_that_fits_hides_nothing() {
        let d = unified_diff("f", Some("a\nb\nc\n"), "a\nc\nd\n");
        assert_eq!(
            (d.hidden_lines, d.hidden_added, d.hidden_removed),
            (0, 0, 0)
        );
        assert!(!d.text.contains("truncated"));
        // Exactly at the limit: still nothing hidden.
        let new: String = (0..MAX_PREVIEW_LINES).map(|i| format!("{i}\n")).collect();
        let d = unified_diff("f", None, &new);
        assert_eq!(d.hidden_lines, 0);
        assert!(!d.text.contains("truncated"));
    }

    /// What the user reads must be what is written: a character that hides, reorders or
    /// overwrites text is shown by its code point instead of being left to the renderer.
    #[test]
    fn control_and_direction_changing_characters_are_shown_by_code_point() {
        // U+202E (right-to-left override) makes "lqs.x" read as "x.sql" on screen.
        assert_eq!(visible("a\u{202E}b"), "a<U+202E>b");
        for (c, shown) in [
            ('\u{0}', "<U+0000>"),
            ('\u{8}', "<U+0008>"),
            ('\u{1b}', "<U+001B>"),
            ('\u{7f}', "<U+007F>"),
            ('\u{85}', "<U+0085>"),
            ('\u{ad}', "<U+00AD>"),
            ('\u{61c}', "<U+061C>"),
            ('\u{200b}', "<U+200B>"),
            ('\u{200d}', "<U+200D>"),
            ('\u{200f}', "<U+200F>"),
            ('\u{202a}', "<U+202A>"),
            ('\u{2028}', "<U+2028>"),
            ('\u{2066}', "<U+2066>"),
            ('\u{2069}', "<U+2069>"),
            ('\u{feff}', "<U+FEFF>"),
            ('\u{e0041}', "<U+E0041>"),
        ] {
            let text = format!("x{c}y");
            assert_eq!(visible(&text), format!("x{shown}y"), "{:?}", c);
            assert_eq!(count_hidden_chars(&text), 1, "{:?}", c);
        }
        // Ordinary text, tabs and line ends (LF and CRLF) are left alone.
        for plain in [
            "select 1;\n\tfrom t\r\n",
            "naïve café 日本語 🙂",
            "ends with cr\r",
        ] {
            assert_eq!(visible(plain), plain);
            assert_eq!(count_hidden_chars(plain), 0, "{plain:?}");
        }
        // A carriage return inside a line returns the cursor to the start of the line.
        assert_eq!(
            visible("drop table t;\rselect 1;   "),
            "drop table t;<U+000D>select 1;   "
        );
        assert_eq!(count_hidden_chars("a\rb\u{202E}c"), 2);
    }

    #[test]
    fn the_diff_shows_such_characters_in_added_removed_and_context_lines_and_in_the_path() {
        let old = "keep\u{200B}\nold\u{1b}[2K\n";
        let new = "keep\u{200B}\nnew \u{202E}txet\n";
        let d = unified_diff("a\u{202E}b.sql", Some(old), new).text;
        assert_eq!(
            d,
            "--- a/a<U+202E>b.sql\n+++ b/a<U+202E>b.sql\n@@ -1,2 +1,2 @@\n keep<U+200B>\n-old<U+001B>[2K\n+new <U+202E>txet\n"
        );
    }
}
