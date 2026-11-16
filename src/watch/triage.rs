//! Triage: compare two watch sessions' logs and find what sets them apart.
//!
//! The question triage answers is "this run broke and that one didn't — what
//! did the broken one say that the good one never did?" A plain `diff` is no
//! use for that: two runs of the same program never print byte-identical
//! lines, because every line carries a timestamp, a pid, a duration, a request
//! id, a port. So lines are compared by their *shape* instead:
//!
//! 1. **Normalize.** Each line has its variable parts masked — timestamps,
//!    dates, times, UUIDs, IP addresses, hex ids and addresses, numbers — and
//!    its whitespace and case folded. `12:01:33.120 GET /api/user/42 in 35ms`
//!    and `12:04:10.002 GET /api/user/7 in 112ms` both become
//!    `<time> get /api/user/<n> in <n>ms`. Lines with the same shape form one
//!    *template*.
//! 2. **Match exactly.** A template both logs have is `common`. Its counts
//!    are kept, because "printed once here, four hundred times there" is often
//!    the finding.
//! 3. **Match fuzzily.** The templates left over on each side are paired with
//!    the most similar leftover on the other, by token-sequence similarity
//!    (`2·LCS / (len_a + len_b)`). A pair above the threshold is `similar`: the
//!    same message saying something different, like `connected to primary` /
//!    `connected to replica`, returned with a word diff.
//! 4. **Whatever is still unmatched is unique** to its log: `only_a` or
//!    `only_b`. Those are usually what you came for.
//!
//! The fuzzy pass is bounded so a pair of 200,000-line logs stays interactive:
//! candidates come from an inverted index over tokens, a cheap multiset bound
//! rules most of them out before the quadratic LCS runs, and very long lines
//! are compared on their first [`MAX_TOKENS`] tokens.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use super::Stream;

/// Lines compared on at most this many tokens; the LCS is quadratic in it.
const MAX_TOKENS: usize = 256;
/// How many candidates per template get a full similarity score.
const MAX_CANDIDATES: usize = 48;
/// A token in more than this share of templates says nothing about which one
/// a line resembles (`the`, `<n>`, `info`), so it isn't used to find candidates.
const COMMON_TOKEN_SHARE: f64 = 0.2;
/// …and nor is one in more templates than this, however many there are.
const COMMON_TOKEN_MAX: usize = 2_000;
/// Candidate votes cast for one template, rarest tokens first. Rare tokens
/// are the ones that pick out a line's true partner.
const VOTES_PER_LINE: usize = 4_096;
/// Candidate votes for a whole comparison. Past it the fuzzy pass stops, and
/// the summary says so, rather than leaving a pathological log spinning.
const VOTE_BUDGET: usize = 20_000_000;

/// What a caller can tune.
#[derive(Debug, Clone)]
pub struct Options {
    /// How similar two templates must be, 0–1, to be paired as `similar`.
    pub threshold: f64,
    /// Line numbers kept per group, per side.
    pub max_seqs: usize,
    /// Groups returned per kind; the summary still counts them all.
    pub max_groups: usize,
    /// Extra text to cut out of every line before comparing — a prefix format
    /// the built-in masks don't know.
    pub ignore: Option<Regex>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threshold: 0.75,
            max_seqs: 200,
            max_groups: 5_000,
            ignore: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    OnlyA,
    OnlyB,
    Similar,
    Common,
}

/// Where one template occurs in one log.
#[derive(Debug, Clone, Serialize)]
pub struct Occurrences {
    /// The template, case preserved — what the group is shown as.
    pub template: String,
    pub count: usize,
    /// How many of those came from stderr.
    pub stderr: usize,
    /// The first [`Options::max_seqs`] line numbers, in order.
    pub seqs: Vec<u64>,
    /// The first occurrence, as printed.
    pub sample: String,
}

/// One piece of a word diff between two templates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffPart {
    /// `same`, `a` (only in A's template) or `b`.
    pub op: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub kind: Kind,
    /// 1.0 for an exact match, 0 for a unique line.
    pub similarity: f64,
    pub a: Option<Occurrences>,
    pub b: Option<Occurrences>,
    /// For `similar`: how A's template differs from B's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<Vec<DiffPart>>,
    /// For `common`: the counts differ enough to be worth a look (at least
    /// double, and by more than one).
    pub count_changed: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Summary {
    pub lines_a: usize,
    pub lines_b: usize,
    pub templates_a: usize,
    pub templates_b: usize,
    pub only_a: usize,
    pub only_b: usize,
    /// Lines (not templates) that are unique to each log.
    pub only_a_lines: usize,
    pub only_b_lines: usize,
    pub similar: usize,
    pub common: usize,
    pub count_changed: usize,
    /// Kinds whose group list was cut at [`Options::max_groups`].
    pub capped: Vec<Kind>,
    /// The fuzzy pass ran out of budget, so some lines counted as unique may
    /// have a similar partner it never got to.
    pub fuzzy_incomplete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub summary: Summary,
    pub groups: Vec<Group>,
}

// ─── Normalization ──────────────────────────────────────────────────────────

/// Every variable part a log line commonly carries, as one alternation so a
/// line is masked in a single pass. Order matters: at a given position the
/// first alternative that matches wins, so whole timestamps are claimed before
/// the dates, times and numbers inside them.
///
/// ASCII classes and word boundaries throughout, and no capture groups: that
/// keeps the search on the regex crate's fast engines, which matters when two
/// 200,000-line logs go through it. [`mask_for`] works out afterwards which
/// alternative a match was.
static MASK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u)",
        // 2026-11-15T10:22:01.123Z, 2026-11-15 10:22:01,123 +0100
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}[T ][0-9]{1,2}:[0-9]{2}(?::[0-9]{2}(?:[.,][0-9]+)?)?(?:[ ]?(?:Z|[+-][0-9]{2}:?[0-9]{2}))?",
        // Nov 15 10:22:01 (syslog)
        r"|\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[ ]+[0-9]{1,2}[ ]+[0-9]{1,2}:[0-9]{2}:[0-9]{2}\b",
        // Dates.
        r"|\b[0-9]{4}[-/.][0-9]{1,2}[-/.][0-9]{1,2}\b|\b[0-9]{1,2}/[0-9]{1,2}/[0-9]{2,4}\b",
        // Times.
        r"|\b[0-9]{1,2}:[0-9]{2}:[0-9]{2}(?:[.,][0-9]+)?\b",
        // UUIDs.
        r"|\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
        // IPv4, with or without a port.
        r"|\b[0-9]{1,3}(?:\.[0-9]{1,3}){3}(?::[0-9]+)?\b",
        // 0x addresses.
        r"|\b0[xX][0-9a-fA-F]+\b",
        // Commit hashes, container ids, trace ids. Checked for a digit in
        // `mask_for` so `deadbeef`-shaped words survive.
        r"|\b[0-9a-fA-F]{8,}\b",
        // Numbers.
        r"|[0-9]+(?:[.,][0-9]+)*",
    ))
    .expect("the mask pattern compiles")
});

/// What a [`MASK`] match stands for, from the shape of the text it matched —
/// or `None` for a hex-looking word with no digits in it, which is a word.
fn mask_for(m: &str) -> Option<&'static str> {
    let has = |c: char| m.contains(c);
    let digits_only = m.bytes().all(|b| b.is_ascii_digit());
    if has(':') && (has('-') || has(' ')) {
        Some("<ts>")
    } else if m.len() == 36 && m.matches('-').count() == 4 {
        Some("<uuid>")
    } else if has('-') || has('/') {
        Some("<date>")
    } else if has(':') {
        Some(if m.matches('.').count() == 3 {
            "<ip>"
        } else {
            "<time>"
        })
    } else if m.starts_with("0x") || m.starts_with("0X") {
        Some("<hex>")
    } else if m.matches('.').count() == 3 && m.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        Some("<ip>")
    } else if digits_only
        || m.bytes()
            .all(|b| b.is_ascii_digit() || b == b'.' || b == b',')
    {
        Some("<n>")
    } else if m.bytes().any(|b| b.is_ascii_digit()) {
        Some("<hex>")
    } else {
        None
    }
}

/// A line's shape: variable parts masked, whitespace collapsed. Case is kept
/// here, for display; [`key`] folds it for matching.
pub fn normalize(text: &str, ignore: Option<&Regex>) -> String {
    let text = match ignore {
        Some(re) => re.replace_all(text, ""),
        None => text.into(),
    };
    let mut masked = String::with_capacity(text.len());
    let mut last = 0;
    for m in MASK.find_iter(&text) {
        if let Some(mask) = mask_for(m.as_str()) {
            masked.push_str(&text[last..m.start()]);
            masked.push_str(mask);
            last = m.end();
        }
    }
    masked.push_str(&text[last..]);

    let mut out = String::with_capacity(masked.len());
    for word in masked.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn key(template: &str) -> String {
    template.to_lowercase()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '<' || c == '>'
}

/// The words of a key, for similarity.
fn tokens(key: &str) -> Vec<&str> {
    key.split(|c: char| !is_word(c))
        .filter(|t| !t.is_empty())
        .take(MAX_TOKENS)
        .collect()
}

/// A template cut into word and non-word runs, for a readable diff.
fn segments(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, c) in text.char_indices() {
        let w = is_word(c);
        if prev.is_some_and(|p| p != w) {
            out.push(&text[start..i]);
            start = i;
        }
        prev = Some(w);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

// ─── Grouping ───────────────────────────────────────────────────────────────

struct Template {
    key: String,
    occ: Occurrences,
}

/// Collapse a log into its templates, in order of first appearance.
fn templates(lines: &[(u64, Stream, String)], opts: &Options) -> Vec<Template> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<Template> = Vec::new();
    for (seq, stream, text) in lines {
        let template = normalize(text, opts.ignore.as_ref());
        if template.is_empty() {
            continue;
        }
        let k = key(&template);
        let i = match index.get(&k) {
            Some(&i) => i,
            None => {
                index.insert(k.clone(), out.len());
                out.push(Template {
                    key: k,
                    occ: Occurrences {
                        template,
                        count: 0,
                        stderr: 0,
                        seqs: Vec::new(),
                        sample: text.trim_end().to_string(),
                    },
                });
                out.len() - 1
            }
        };
        let occ = &mut out[i].occ;
        occ.count += 1;
        if *stream == Stream::Stderr {
            occ.stderr += 1;
        }
        if occ.seqs.len() < opts.max_seqs {
            occ.seqs.push(*seq);
        }
    }
    out
}

// ─── Similarity ─────────────────────────────────────────────────────────────

/// Length of the longest common subsequence of two token lists.
fn lcs(a: &[&str], b: &[&str]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let mut row = vec![0usize; b.len() + 1];
    for x in a {
        let mut diag = 0;
        for (j, y) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if x == y { diag + 1 } else { up.max(row[j]) };
            diag = up;
        }
    }
    row[b.len()]
}

/// `2·LCS / (len_a + len_b)`: 1 for identical token lists, 0 for disjoint.
/// Order-aware, so `a failed b` and `b failed a` aren't called the same.
pub fn similarity(a: &[&str], b: &[&str]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * lcs(a, b) as f64 / total as f64
}

/// How many tokens two multisets share. An upper bound on the LCS, and far
/// cheaper, so it rules candidates out before the LCS runs.
fn shared(a: &HashMap<&str, usize>, b: &HashMap<&str, usize>) -> usize {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small
        .iter()
        .map(|(t, n)| (*n).min(large.get(t).copied().unwrap_or(0)))
        .sum()
}

fn bag<'a>(tokens: &[&'a str]) -> HashMap<&'a str, usize> {
    let mut m = HashMap::new();
    for t in tokens {
        *m.entry(*t).or_insert(0) += 1;
    }
    m
}

/// For each of `from`, its most similar of `to` at or above `threshold`.
/// `budget` is the candidate votes left to spend; it's cleared to zero, and the
/// rest of `from` left unmatched, when it runs out.
fn best_matches(
    from: &[Vec<&str>],
    to: &[Vec<&str>],
    threshold: f64,
    budget: &mut usize,
) -> Vec<Option<(usize, f64)>> {
    let to_bags: Vec<HashMap<&str, usize>> = to.iter().map(|t| bag(t)).collect();

    // Token → the `to` templates that contain it.
    let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, b) in to_bags.iter().enumerate() {
        for t in b.keys() {
            postings.entry(*t).or_default().push(i);
        }
    }
    let common_limit =
        ((to.len() as f64 * COMMON_TOKEN_SHARE) as usize).clamp(64, COMMON_TOKEN_MAX);

    // Votes are tallied in a dense array and reset through `touched`, rather
    // than a fresh map per line: this loop is the hot one.
    let mut tally = vec![0u32; to.len()];
    let mut touched: Vec<usize> = Vec::new();

    from.iter()
        .map(|a| {
            if a.is_empty() || *budget == 0 {
                return None;
            }
            let a_bag = bag(a);
            let mut lists: Vec<&Vec<usize>> = a_bag
                .keys()
                .filter_map(|t| postings.get(t))
                .filter(|list| list.len() <= common_limit)
                .collect();
            lists.sort_by_key(|list| list.len());

            let mut cast = 0;
            for list in lists {
                if cast + list.len() > VOTES_PER_LINE.min(*budget) {
                    break;
                }
                cast += list.len();
                for &i in list {
                    if tally[i] == 0 {
                        touched.push(i);
                    }
                    tally[i] += 1;
                }
            }
            *budget = budget.saturating_sub(cast.max(1));
            let mut candidates: Vec<(usize, u32)> = touched
                .drain(..)
                .map(|i| (i, std::mem::take(&mut tally[i])))
                .collect();
            if candidates.len() > MAX_CANDIDATES {
                candidates.select_nth_unstable_by(MAX_CANDIDATES, |x, y| {
                    y.1.cmp(&x.1).then(x.0.cmp(&y.0))
                });
                candidates.truncate(MAX_CANDIDATES);
            }
            candidates.sort_unstable_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));

            let mut best: Option<(usize, f64)> = None;
            for (i, _) in candidates {
                let b = &to[i];
                let total = (a.len() + b.len()) as f64;
                // Neither the length difference nor the shared tokens can
                // reach the threshold: skip the LCS.
                if 2.0 * a.len().min(b.len()) as f64 / total < threshold {
                    continue;
                }
                let bound = 2.0 * shared(&a_bag, &to_bags[i]) as f64 / total;
                if bound < threshold || best.is_some_and(|(_, s)| bound <= s) {
                    continue;
                }
                let score = similarity(a, b);
                if score >= threshold && best.is_none_or(|(_, s)| score > s) {
                    best = Some((i, score));
                }
            }
            best
        })
        .collect()
}

/// A word diff of two templates.
pub fn diff(a: &str, b: &str) -> Vec<DiffPart> {
    let sa = segments(a);
    let sb = segments(b);
    if sa.len() > 4 * MAX_TOKENS || sb.len() > 4 * MAX_TOKENS {
        return vec![
            DiffPart {
                op: "a",
                text: a.to_string(),
            },
            DiffPart {
                op: "b",
                text: b.to_string(),
            },
        ];
    }
    let eq = |x: &str, y: &str| x.eq_ignore_ascii_case(y);

    // table[i][j] = LCS of sa[i..], sb[j..].
    let (n, m) = (sa.len(), sb.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if eq(sa[i], sb[j]) {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut parts: Vec<DiffPart> = Vec::new();
    let mut push = |op: &'static str, text: &str| match parts.last_mut() {
        Some(last) if last.op == op => last.text.push_str(text),
        _ => parts.push(DiffPart {
            op,
            text: text.to_string(),
        }),
    };
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if eq(sa[i], sb[j]) {
            push("same", sa[i]);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            push("a", sa[i]);
            i += 1;
        } else {
            push("b", sb[j]);
            j += 1;
        }
    }
    for s in &sa[i..] {
        push("a", s);
    }
    for s in &sb[j..] {
        push("b", s);
    }
    parts
}

// ─── Comparison ─────────────────────────────────────────────────────────────

/// Compare two logs, each given as `(seq, stream, plain text)` lines.
pub fn compare(a: &[(u64, Stream, String)], b: &[(u64, Stream, String)], opts: &Options) -> Report {
    let ta = templates(a, opts);
    let tb = templates(b, opts);

    let mut summary = Summary {
        lines_a: a.len(),
        lines_b: b.len(),
        templates_a: ta.len(),
        templates_b: tb.len(),
        ..Summary::default()
    };

    let b_index: HashMap<&str, usize> = tb
        .iter()
        .enumerate()
        .map(|(i, t)| (t.key.as_str(), i))
        .collect();

    // Exact matches.
    let mut common: Vec<Group> = Vec::new();
    let mut b_matched = vec![false; tb.len()];
    let mut a_left: Vec<usize> = Vec::new();
    for (i, t) in ta.iter().enumerate() {
        match b_index.get(t.key.as_str()) {
            Some(&j) => {
                b_matched[j] = true;
                let (ca, cb) = (t.occ.count, tb[j].occ.count);
                let (lo, hi) = (ca.min(cb), ca.max(cb));
                let count_changed = hi >= lo * 2 && hi - lo > 1;
                common.push(Group {
                    kind: Kind::Common,
                    similarity: 1.0,
                    a: Some(t.occ.clone()),
                    b: Some(tb[j].occ.clone()),
                    diff: None,
                    count_changed,
                });
            }
            None => a_left.push(i),
        }
    }
    let b_left: Vec<usize> = (0..tb.len()).filter(|&j| !b_matched[j]).collect();

    // Fuzzy matches between the leftovers, both ways round: B's best partner
    // for an A line may already be paired, and the B line left behind can
    // still have a close A line of its own.
    let a_tokens: Vec<Vec<&str>> = a_left.iter().map(|&i| tokens(&ta[i].key)).collect();
    let b_tokens: Vec<Vec<&str>> = b_left.iter().map(|&j| tokens(&tb[j].key)).collect();
    let mut budget = VOTE_BUDGET;
    let forward = best_matches(&a_tokens, &b_tokens, opts.threshold, &mut budget);

    let mut pairs: Vec<(usize, usize, f64)> = Vec::new(); // indices into a_left / b_left
    let mut a_paired = vec![false; a_left.len()];
    let mut b_paired = vec![false; b_left.len()];
    for (x, m) in forward.iter().enumerate() {
        if let Some((y, s)) = *m {
            pairs.push((x, y, s));
            a_paired[x] = true;
            b_paired[y] = true;
        }
    }
    let unpaired_b: Vec<usize> = (0..b_left.len()).filter(|&y| !b_paired[y]).collect();
    let unpaired_b_tokens: Vec<Vec<&str>> =
        unpaired_b.iter().map(|&y| b_tokens[y].clone()).collect();
    let backward = best_matches(&unpaired_b_tokens, &a_tokens, opts.threshold, &mut budget);
    summary.fuzzy_incomplete = budget == 0;
    for (k, m) in backward.iter().enumerate() {
        if let Some((x, s)) = *m {
            let y = unpaired_b[k];
            pairs.push((x, y, s));
            a_paired[x] = true;
            b_paired[y] = true;
        }
    }

    let mut similar: Vec<Group> = pairs
        .into_iter()
        .map(|(x, y, s)| {
            let (oa, ob) = (&ta[a_left[x]].occ, &tb[b_left[y]].occ);
            Group {
                kind: Kind::Similar,
                similarity: s,
                diff: Some(diff(&oa.template, &ob.template)),
                a: Some(oa.clone()),
                b: Some(ob.clone()),
                count_changed: false,
            }
        })
        .collect();
    similar.sort_by_key(|g| g.a.as_ref().map(|o| o.seqs.first().copied().unwrap_or(0)));

    let unique = |occ: &Occurrences, kind: Kind| Group {
        kind,
        similarity: 0.0,
        a: (kind == Kind::OnlyA).then(|| occ.clone()),
        b: (kind == Kind::OnlyB).then(|| occ.clone()),
        diff: None,
        count_changed: false,
    };
    let only_a: Vec<Group> = (0..a_left.len())
        .filter(|&x| !a_paired[x])
        .map(|x| unique(&ta[a_left[x]].occ, Kind::OnlyA))
        .collect();
    let only_b: Vec<Group> = (0..b_left.len())
        .filter(|&y| !b_paired[y])
        .map(|y| unique(&tb[b_left[y]].occ, Kind::OnlyB))
        .collect();

    // Changed counts first: the common lines worth reading.
    common.sort_by(|x, y| {
        y.count_changed.cmp(&x.count_changed).then_with(|| {
            let first = |g: &Group| g.a.as_ref().and_then(|o| o.seqs.first().copied());
            first(x).cmp(&first(y))
        })
    });

    summary.only_a = only_a.len();
    summary.only_b = only_b.len();
    summary.only_a_lines = only_a
        .iter()
        .filter_map(|g| g.a.as_ref())
        .map(|o| o.count)
        .sum();
    summary.only_b_lines = only_b
        .iter()
        .filter_map(|g| g.b.as_ref())
        .map(|o| o.count)
        .sum();
    summary.similar = similar.len();
    summary.common = common.len();
    summary.count_changed = common.iter().filter(|g| g.count_changed).count();

    let mut groups = Vec::new();
    for (kind, mut list) in [
        (Kind::OnlyA, only_a),
        (Kind::OnlyB, only_b),
        (Kind::Similar, similar),
        (Kind::Common, common),
    ] {
        if list.len() > opts.max_groups {
            list.truncate(opts.max_groups);
            summary.capped.push(kind);
        }
        groups.extend(list);
    }

    Report { summary, groups }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(lines: &[&str]) -> Vec<(u64, Stream, String)> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u64 + 1, Stream::Stdout, l.to_string()))
            .collect()
    }

    fn of_kind(report: &Report, kind: Kind) -> Vec<&Group> {
        report.groups.iter().filter(|g| g.kind == kind).collect()
    }

    #[test]
    fn timestamps_and_numbers_do_not_make_a_line_unique() {
        assert_eq!(
            normalize("2026-11-15T10:22:01.123Z INFO request 42 took 35ms", None),
            "<ts> INFO request <n> took <n>ms"
        );
        assert_eq!(
            normalize("[10:22:01] pid=991 addr=0x7ffe12 id 3f2a9c1e0b", None),
            "[<time>] pid=<n> addr=<hex> id <hex>"
        );
        assert_eq!(
            normalize("Nov 15 10:22:01 host sshd: from 10.0.0.12:5522", None),
            "<ts> host sshd: from <ip>"
        );
        assert_eq!(
            normalize("job 6f1c2b8e-1d2a-4c3b-9e8f-0a1b2c3d4e5f   done", None),
            "job <uuid> done"
        );
    }

    #[test]
    fn hex_shaped_words_without_digits_survive() {
        assert_eq!(normalize("deadbeefcafe face", None), "deadbeefcafe face");
    }

    #[test]
    fn a_custom_ignore_pattern_is_cut_first() {
        let re = Regex::new(r"^\[[a-z-]+\]\s*").unwrap();
        assert_eq!(normalize("[worker-a] ready", Some(&re)), "ready");
    }

    #[test]
    fn the_same_lines_at_different_times_are_common() {
        let a = log(&["10:00:01 starting server on port 8080", "10:00:02 ready"]);
        let b = log(&["11:30:45 starting server on port 9090", "11:30:47 ready"]);
        let report = compare(&a, &b, &Options::default());
        assert_eq!(report.summary.common, 2);
        assert_eq!(report.summary.only_a + report.summary.only_b, 0);
    }

    #[test]
    fn a_line_only_one_log_printed_is_unique_to_it() {
        let a = log(&["boot", "loading config from /etc/app.toml", "ready"]);
        let b = log(&[
            "boot",
            "loading config from /etc/app.toml",
            "panic: index out of range",
            "ready",
        ]);
        let report = compare(&a, &b, &Options::default());
        let only_b = of_kind(&report, Kind::OnlyB);
        assert_eq!(only_b.len(), 1);
        assert_eq!(
            only_b[0].b.as_ref().unwrap().sample,
            "panic: index out of range"
        );
        assert_eq!(only_b[0].b.as_ref().unwrap().seqs, vec![3]);
        assert!(of_kind(&report, Kind::OnlyA).is_empty());
    }

    #[test]
    fn a_reworded_line_is_similar_with_a_diff() {
        let a = log(&["connected to database primary at host db1 in region east"]);
        let b = log(&["connected to database replica at host db1 in region east"]);
        let report = compare(&a, &b, &Options::default());
        let similar = of_kind(&report, Kind::Similar);
        assert_eq!(similar.len(), 1, "{:#?}", report.groups);
        let diff = similar[0].diff.as_ref().unwrap();
        assert!(diff.iter().any(|p| p.op == "a" && p.text == "primary"));
        assert!(diff.iter().any(|p| p.op == "b" && p.text == "replica"));
    }

    #[test]
    fn unrelated_lines_are_not_paired() {
        let a = log(&["compiling crate serde"]);
        let b = log(&["error: linker cc not found"]);
        let report = compare(&a, &b, &Options::default());
        assert_eq!(report.summary.similar, 0);
        assert_eq!(report.summary.only_a, 1);
        assert_eq!(report.summary.only_b, 1);
    }

    #[test]
    fn a_big_change_in_how_often_a_line_appears_is_flagged() {
        let a = log(&["retrying connection", "ok"]);
        let mut lines = vec!["retrying connection"; 10];
        lines.push("ok");
        let b = log(&lines);
        let report = compare(&a, &b, &Options::default());
        let changed: Vec<_> = of_kind(&report, Kind::Common)
            .into_iter()
            .filter(|g| g.count_changed)
            .collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].b.as_ref().unwrap().count, 10);
        assert_eq!(report.summary.count_changed, 1);
    }

    #[test]
    fn blank_lines_are_ignored() {
        let a = log(&["", "   ", "hello"]);
        let b = log(&["hello"]);
        let report = compare(&a, &b, &Options::default());
        assert_eq!(report.summary.templates_a, 1);
        assert_eq!(report.summary.common, 1);
    }

    #[test]
    fn similarity_respects_order() {
        let x = ["a", "failed", "b"];
        let y = ["b", "failed", "a"];
        assert!(similarity(&x, &y) < 0.7);
        assert_eq!(similarity(&x, &x), 1.0);
    }

    #[test]
    fn groups_are_capped_but_still_counted() {
        let a: Vec<String> = (0..20)
            .map(|i| format!("only-a-word-{}", ('a'..='z').nth(i).unwrap()))
            .collect();
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        let report = compare(
            &log(&a),
            &log(&[]),
            &Options {
                max_groups: 5,
                ..Options::default()
            },
        );
        assert_eq!(report.summary.only_a, 20);
        assert_eq!(of_kind(&report, Kind::OnlyA).len(), 5);
        assert_eq!(report.summary.capped, vec![Kind::OnlyA]);
    }
}
