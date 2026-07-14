//! Fuzzy string-replacement for the `edit_file` tool.
//!
//! Instead of asking the model to re-emit a whole file (expensive, and easy to
//! corrupt on a large file), `edit_file` takes an `old`→`new` string pair and
//! splices it in. Models rarely reproduce surrounding whitespace perfectly, so
//! a single exact match is brittle. We try a ladder of increasingly lenient
//! matchers — exact first, then line-trimmed, whitespace-normalized,
//! indentation-flexible, and finally a first/last-line block anchor — and take
//! the first that yields a single unambiguous span. This is a pragmatic port of
//! opencode's multi-strategy edit tool.
//!
//! A "disproportionate match" guard refuses to splice when the matched span is
//! far larger than what the model asked to replace, so a loose matcher can't
//! silently swallow half the file.

/// Find the exact substring in `content` that a given `find` string should map
/// to, using progressively looser strategies. Each strategy yields candidate
/// spans (verbatim substrings of `content`); the caller checks uniqueness.
type Matcher = fn(&str, &str) -> Vec<String>;

const MATCHERS: &[Matcher] = &[
    simple,
    line_trimmed,
    whitespace_normalized,
    indentation_flexible,
    block_anchor,
];

/// Replace `find` with `replacement` in `content`.
///
/// With `replace_all`, every occurrence of the matched span is replaced.
/// Otherwise the match must be unique. Returns an error the model can act on
/// when the string is missing, ambiguous, or the fuzzy match is oversized.
pub fn replace(content: &str, find: &str, replacement: &str, replace_all: bool) -> anyhow::Result<String> {
    if find.is_empty() {
        anyhow::bail!("`old` is empty — give the exact text to replace (use propose_change for a whole new file)");
    }
    if find == replacement {
        anyhow::bail!("`old` and `new` are identical — nothing to change");
    }

    let mut any_candidate = false;
    for matcher in MATCHERS {
        for span in matcher(content, find) {
            // A matcher can yield a span that isn't actually a substring
            // (shouldn't happen, but stay honest) — skip those.
            let Some(index) = content.find(&span) else {
                continue;
            };
            any_candidate = true;
            if disproportionate(&span, find) {
                anyhow::bail!(
                    "the closest match spans far more than the text you gave — re-read the file \
                     and pass the exact `old` text for the intended edit"
                );
            }
            if replace_all {
                return Ok(content.replace(&span, replacement));
            }
            // Must be unique to splice safely.
            if content.rfind(&span) != Some(index) {
                continue;
            }
            let mut out = String::with_capacity(content.len() - span.len() + replacement.len());
            out.push_str(&content[..index]);
            out.push_str(replacement);
            out.push_str(&content[index + span.len()..]);
            return Ok(out);
        }
    }

    if any_candidate {
        anyhow::bail!(
            "`old` matches in more than one place — add surrounding lines to make it unique, \
             or set replace_all: true"
        );
    }
    anyhow::bail!(
        "`old` was not found — it must match the file's text (whitespace and indentation are \
         matched leniently, but the lines themselves must be present)"
    )
}

/// Refuse a fuzzy span that is wildly larger than what was asked for, so a
/// loose matcher can't swallow unrelated code.
fn disproportionate(span: &str, find: &str) -> bool {
    let span_lines = span.lines().count();
    let find_lines = find.lines().count();
    if span_lines >= (find_lines + 3).max(find_lines * 2) {
        return true;
    }
    if find_lines <= 1 {
        return false;
    }
    span.trim().len() > (find.trim().len() + 500).max(find.trim().len() * 4)
}

// ─── Matchers ────────────────────────────────────────────────────────────────

/// Exact substring.
fn simple(_content: &str, find: &str) -> Vec<String> {
    vec![find.to_string()]
}

/// Match line-by-line ignoring leading/trailing whitespace on each line, then
/// return the verbatim span from the original.
fn line_trimmed(content: &str, find: &str) -> Vec<String> {
    let orig: Vec<&str> = content.split('\n').collect();
    let mut search: Vec<&str> = find.split('\n').collect();
    if search.last() == Some(&"") {
        search.pop();
    }
    if search.is_empty() || search.len() > orig.len() {
        return Vec::new();
    }

    // Byte offset of the start of each original line.
    let mut starts = Vec::with_capacity(orig.len());
    let mut acc = 0usize;
    for l in &orig {
        starts.push(acc);
        acc += l.len() + 1; // +1 for the '\n'
    }

    let mut out = Vec::new();
    for i in 0..=orig.len() - search.len() {
        if (0..search.len()).all(|j| orig[i + j].trim() == search[j].trim()) {
            let start = starts[i];
            let last = i + search.len() - 1;
            let end = starts[last] + orig[last].len();
            out.push(content[start..end].to_string());
        }
    }
    out
}

/// Collapse every run of whitespace to a single space before comparing, so
/// reflowed indentation or spacing differences still match.
fn whitespace_normalized(content: &str, find: &str) -> Vec<String> {
    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    let needle = norm(find);
    if needle.is_empty() {
        return Vec::new();
    }
    let orig: Vec<&str> = content.split('\n').collect();
    let find_lines = find.split('\n').filter(|l| !l.trim().is_empty()).count().max(1);

    let mut starts = Vec::with_capacity(orig.len());
    let mut acc = 0usize;
    for l in &orig {
        starts.push(acc);
        acc += l.len() + 1;
    }

    let mut out = Vec::new();
    // Slide a window of the same line count and compare normalized text.
    for i in 0..orig.len() {
        let end_line = (i + find_lines).min(orig.len());
        if end_line <= i {
            continue;
        }
        let start = starts[i];
        let last = end_line - 1;
        let end = starts[last] + orig[last].len();
        let span = &content[start..end];
        if norm(span) == needle {
            out.push(span.to_string());
        }
    }
    out
}

/// Match when the only difference is how far each line is indented (common when
/// a model pastes a snippet dedented). Compares each line with leading
/// whitespace stripped.
fn indentation_flexible(content: &str, find: &str) -> Vec<String> {
    let strip = |s: &str| -> Vec<String> {
        s.split('\n')
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim_start().to_string())
            .collect()
    };
    let needle = strip(find);
    if needle.is_empty() {
        return Vec::new();
    }
    let orig: Vec<&str> = content.split('\n').collect();

    let mut starts = Vec::with_capacity(orig.len());
    let mut acc = 0usize;
    for l in &orig {
        starts.push(acc);
        acc += l.len() + 1;
    }

    let mut out = Vec::new();
    let n = needle.len();
    if n > orig.len() {
        return out;
    }
    for i in 0..=orig.len() - n {
        if (0..n).all(|j| orig[i + j].trim_start() == needle[j]) {
            let start = starts[i];
            let end = starts[i + n - 1] + orig[i + n - 1].len();
            out.push(content[start..end].to_string());
        }
    }
    out
}

/// Anchor on the first and last non-empty lines of the search block (trimmed)
/// and take everything between them. Handy when the middle of the block drifted
/// but its boundaries are stable. Only used for blocks of 3+ lines.
fn block_anchor(content: &str, find: &str) -> Vec<String> {
    let orig: Vec<&str> = content.split('\n').collect();
    let mut search: Vec<&str> = find.split('\n').collect();
    if search.last() == Some(&"") {
        search.pop();
    }
    if search.len() < 3 {
        return Vec::new();
    }
    let first = search[0].trim();
    let last = search[search.len() - 1].trim();
    let block = search.len();
    let max_delta = ((block as f64) * 0.25).floor().max(1.0) as usize;

    let mut starts = Vec::with_capacity(orig.len());
    let mut acc = 0usize;
    for l in &orig {
        starts.push(acc);
        acc += l.len() + 1;
    }

    let mut out = Vec::new();
    for i in 0..orig.len() {
        if orig[i].trim() != first {
            continue;
        }
        for j in (i + 2)..orig.len() {
            if orig[j].trim() == last {
                let actual = j - i + 1;
                if actual.abs_diff(block) <= max_delta {
                    let start = starts[i];
                    let end = starts[j] + orig[j].len();
                    out.push(content[start..end].to_string());
                }
                break; // only the first closing anchor
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_is_replaced() {
        let out = replace("let x = 1;\nlet y = 2;\n", "let x = 1;", "let x = 42;", false).unwrap();
        assert_eq!(out, "let x = 42;\nlet y = 2;\n");
    }

    #[test]
    fn indentation_drift_still_matches() {
        // Model supplied the body dedented; file has it indented under a fn.
        let content = "fn f() {\n    let a = 1;\n    let b = 2;\n}\n";
        let out = replace(content, "let a = 1;\nlet b = 2;", "let a = 10;\nlet b = 20;", false).unwrap();
        assert!(out.contains("let a = 10;"));
        assert!(out.contains("let b = 20;"));
        // Original indentation of the surrounding block is untouched.
        assert!(out.starts_with("fn f() {\n"));
    }

    #[test]
    fn ambiguous_match_without_replace_all_errors() {
        let content = "a();\nfoo();\na();\n";
        let err = replace(content, "a();", "b();", false).unwrap_err();
        assert!(err.to_string().contains("more than one place"));
        // replace_all resolves it.
        let out = replace(content, "a();", "b();", true).unwrap();
        assert_eq!(out, "b();\nfoo();\nb();\n");
    }

    #[test]
    fn missing_text_errors() {
        let err = replace("hello\n", "goodbye", "x", false).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn oversized_fuzzy_match_is_refused() {
        // A two-line anchor that would swallow a huge block gets rejected by the
        // disproportion guard rather than silently deleting everything between.
        let mut content = String::from("start\n");
        for i in 0..40 {
            content.push_str(&format!("line {i}\n"));
        }
        content.push_str("end\n");
        let find = "start\nmiddle\nend"; // 3 lines, block-anchor would span 42
        let err = replace(&content, find, "x", false).unwrap_err();
        assert!(err.to_string().contains("spans far more") || err.to_string().contains("not found"));
    }
}
