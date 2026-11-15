//! Tiny line-based unified diff used for `write_file` approval previews.

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

/// Unified diff of `old` (None = the file does not exist yet) against `new`.
pub(super) fn unified_diff(path: &str, old: Option<&str>, new: &str) -> String {
    let old_lines = split_lines(old.unwrap_or(""));
    let new_lines = split_lines(new);
    if old.is_some() && old_lines == new_lines {
        return format!("No changes: {path} already has this content.");
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
    'hunks: for (s, e) in ranges {
        let slice = &ops[s..=e];
        let old_len = slice.iter().filter(|o| o.kind != Kind::Insert).count();
        let new_len = slice.iter().filter(|o| o.kind != Kind::Delete).count();
        let old_start = if old_len == 0 { a_idx[s] } else { a_idx[s] + 1 };
        let new_start = if new_len == 0 { b_idx[s] } else { b_idx[s] + 1 };
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for op in slice {
            if emitted >= MAX_PREVIEW_LINES {
                out.push_str("… diff truncated for display …\n");
                break 'hunks;
            }
            let sign = match op.kind {
                Kind::Equal => ' ',
                Kind::Delete => '-',
                Kind::Insert => '+',
            };
            out.push(sign);
            out.push_str(op.text);
            out.push('\n');
            emitted += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_file() {
        let d = unified_diff("a.sql", None, "select 1;\nselect 2;\n");
        assert_eq!(
            d,
            "--- /dev/null\n+++ b/a.sql\n@@ -0,0 +1,2 @@\n+select 1;\n+select 2;\n"
        );
    }

    #[test]
    fn unchanged() {
        assert!(unified_diff("a.sql", Some("x\n"), "x\n").starts_with("No changes"));
        // Trailing-newline-only differences are not shown as changes.
        assert!(unified_diff("a.sql", Some("x"), "x\n").starts_with("No changes"));
    }

    #[test]
    fn single_line_change_with_context() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let new = "1\n2\n3\n4\nFIVE\n6\n7\n8\n9\n10\n";
        let d = unified_diff("f.sql", Some(old), new);
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
        let d = unified_diff("f", Some(&old.join("\n")), &new.join("\n"));
        assert_eq!(d.matches("@@ -").count(), 2, "{d}");
        assert!(d.contains("-2\n+two\n"));
        assert!(d.contains("-28\n+twenty-eight\n"));
    }

    #[test]
    fn insert_and_delete() {
        let d = unified_diff("f", Some("a\nb\nc\n"), "a\nc\nd\n");
        assert!(d.contains("-b\n"));
        assert!(d.contains("+d\n"));
        assert!(d.contains(" a\n"));
    }

    #[test]
    fn huge_rewrite_is_capped() {
        let old: String = (0..5000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..5000).map(|i| format!("new {i}\n")).collect();
        let d = unified_diff("big", Some(&old), &new);
        assert!(d.contains("diff truncated"));
        assert!(d.lines().count() < 700);
    }
}
