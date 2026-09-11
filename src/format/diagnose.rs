//! Turning a parser's complaint into something a person can act on.
//!
//! `serde_yaml_ng` and `toml` both report real, accurate errors — "did not find
//! expected '-' indicator at line 5 column 4" is precisely true. It is also
//! useless to somebody who has written eighty lines of YAML and cannot see
//! which of them the parser means, because the one thing the message doesn't
//! contain is *the line*.
//!
//! So every parse failure on a ciabatta config goes through here and comes back
//! as: the file, the offending line quoted with its neighbours, a caret under
//! the column, the parser's own words, and — where the shape of the error says
//! what went wrong — a sentence on how to fix it.
//!
//! The hints are deliberately keyed off the parser's message rather than off a
//! re-parse of the document. Recovering the author's intent from a broken file
//! is guesswork; recognizing "this is the tabs error" from the text of the
//! tabs error is not.

use std::fmt::Write as _;
use std::path::Path;

use super::Format;

/// How many lines of context are quoted either side of the error.
const CONTEXT: usize = 2;

/// A position in the source, 1-based as the parsers report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// Build the full diagnostic for a failed parse.
///
/// `path` is what the file is called — the error names it, so the caller does
/// not need to add a `.context()` saying the same thing a second time.
pub fn report(
    path: Option<&Path>,
    content: &str,
    format: Format,
    message: &str,
    location: Option<Location>,
) -> String {
    let mut out = String::new();

    let name = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| format!("this {}", format.ext()));

    // The headline says what kind of problem it is, because "syntax" and
    // "shape" send you to different parts of the file. A syntax error is on the
    // line the caret points at; a schema error is about the *value* there,
    // which is usually correct YAML in the wrong place.
    let kind = classify(message);
    let _ = match kind {
        Kind::Syntax => writeln!(out, "{name} is not valid {}.", format.ext().to_uppercase()),
        Kind::Schema => writeln!(
            out,
            "{name} is valid {} but not a valid ciabatta config.",
            format.ext().to_uppercase()
        ),
    };

    if let Some(location) = location {
        let excerpt = excerpt(content, location);
        if !excerpt.is_empty() {
            let _ = write!(out, "\n{excerpt}");
        }
    }

    // The parser's own words, with the trailing location stripped: the caret
    // above already showed where, and repeating "at line 5 column 4" after an
    // excerpt that points at line 5 column 4 reads as a second, different error.
    let _ = write!(out, "\n{}", strip_location(message));

    // The stripped message, so a hint quoting the parser's words back doesn't
    // re-append the location the caret already showed.
    if let Some(hint) = hint(strip_location(message), content, location) {
        let _ = write!(out, "\n\n{hint}");
    }

    out
}

/// Whether the parser is complaining about the text or about its meaning.
enum Kind {
    Syntax,
    Schema,
}

/// Schema failures come from serde and are phrased in serde's vocabulary; YAML
/// and TOML syntax failures come from the tokenizer and are phrased in the
/// spec's. The two vocabularies don't overlap, so the message itself says which
/// layer rejected the file.
fn classify(message: &str) -> Kind {
    const SERDE: &[&str] = &[
        "invalid type",
        "invalid value",
        "invalid length",
        "unknown field",
        "unknown variant",
        "missing field",
        "duplicate field",
        "expected a string",
        "expected a sequence",
        "expected a map",
        "data did not match",
    ];
    if SERDE.iter().any(|needle| message.contains(needle)) {
        Kind::Schema
    } else {
        Kind::Syntax
    }
}

/// The offending line and its neighbours, with a caret under the column.
///
/// Line numbers are right-aligned in a gutter so the source stays aligned with
/// itself, which matters more here than in most excerpts: YAML errors are
/// nearly always errors of indentation, and an excerpt that doesn't preserve
/// the indentation hides the very thing it was printed to show.
fn excerpt(content: &str, location: Location) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() || location.line == 0 || location.line > lines.len() {
        return String::new();
    }

    let target = location.line - 1;
    let first = target.saturating_sub(CONTEXT);
    let last = (target + CONTEXT).min(lines.len() - 1);
    let width = (last + 1).to_string().len();

    let mut out = String::new();
    for (index, line) in lines.iter().enumerate().take(last + 1).skip(first) {
        let number = index + 1;
        // A tab in the source would shift the caret away from the character it
        // is pointing at, since the gutter is one column per character. They're
        // also illegal as YAML indentation, so a file containing one is a file
        // being diagnosed — showing them expanded is both truer and readable.
        let shown = line.replace('\t', "    ");
        let marker = if index == target { ">" } else { " " };
        let _ = writeln!(out, "  {marker} {number:>width$} | {shown}");

        if index == target {
            // The column the parser names counts characters from 1, and the
            // tab expansion above moved every character after a tab. Count the
            // caret's offset through the same expansion so it still lands.
            let before = line
                .chars()
                .take(location.column.saturating_sub(1))
                .fold(0usize, |n, c| n + if c == '\t' { 4 } else { 1 });
            let _ = writeln!(
                out,
                "  {:width$} | {}^",
                "",
                " ".repeat(before),
                width = width + 2
            );
        }
    }
    out
}

/// Drop the `at line N column M` the parsers append.
fn strip_location(message: &str) -> &str {
    match message.find(" at line ") {
        // "while parsing a block collection at line 3 column 3" is a *second*
        // location naming where the construct opened, and that one is worth
        // keeping — it is half the answer to an indentation error. Only a
        // trailing location with nothing after it is redundant with the caret.
        Some(index) if message[index..].split_whitespace().count() <= 5 => {
            message[..index].trim_end()
        }
        _ => message,
    }
}

/// A sentence on how to fix this particular failure, when its shape says one.
///
/// Silence is a real option here: a wrong hint on a confusing error is worse
/// than no hint, because it sends somebody to look at the thing that is fine.
fn hint(message: &str, content: &str, location: Option<Location>) -> Option<String> {
    let line = location
        .and_then(|l| content.lines().nth(l.line.saturating_sub(1)))
        .unwrap_or("");

    // Tabs first: they produce several different downstream messages, and every
    // one of them is a lie about what is wrong.
    if line.contains('\t')
        && line[..line.find(|c: char| c != ' ' && c != '\t').unwrap_or(0)].contains('\t')
    {
        return Some(
            "This line is indented with a tab. YAML does not allow tabs for indentation \
             anywhere — replace them with spaces."
                .into(),
        );
    }

    // An unterminated quote swallows the following lines into one scalar, so
    // the parser complains about whatever it eventually choked on — several
    // lines below the quote, and phrased as an indentation problem. Checked
    // before the indentation hint for exactly that reason: that hint is right
    // far more often, and wrong in precisely this case.
    if let Some(opened) = unclosed_quote(content, location) {
        return Some(format!(
            "The quote opened on line {opened} is never closed, so everything after it \
             was read as one value and the parser only noticed further down. Close it, \
             or escape the quote inside it."
        ));
    }

    if message.contains("did not find expected '-' indicator")
        || message.contains("did not find expected key")
        || message.contains("did not find expected node content")
    {
        return Some(
            "YAML is indentation-sensitive: every entry in a list, and every key in a \
             mapping, has to start at the same column as its siblings. Check that this \
             line lines up with the ones above it."
                .into(),
        );
    }

    if message.contains("mapping values are not allowed") {
        return Some(
            "There is a second `:` on this line. A value containing a colon has to be \
             quoted — write `run: \"echo a: b\"` rather than `run: echo a: b`."
                .into(),
        );
    }

    if message.contains("duplicate entry") || message.contains("duplicate field") {
        return Some(
            "The same key is set twice in this mapping. Keep one — the second would \
             silently win, which is rarely what was meant."
                .into(),
        );
    }

    if message.contains("found unexpected end of stream")
        || message.contains("while parsing a quoted scalar")
    {
        return Some(
            "A quote or bracket opened earlier is never closed, so the parser ran off \
             the end of the file looking for it."
                .into(),
        );
    }

    if let Some(field) = after(message, "unknown field `") {
        return Some(format!(
            "`{field}` is not a key ciabatta understands here. Check it for a typo — \
             and note the keys are singular or plural exactly as documented \
             (`steps:`, not `step:`)."
        ));
    }

    if let Some(field) = after(message, "missing field `") {
        return Some(format!(
            "`{field}` is required here and the file does not set it."
        ));
    }

    // "invalid type: sequence, expected a string" — the commonest schema error
    // by a distance, and the one whose stock phrasing reads most like noise.
    if let Some(rest) = after_str(message, "invalid type: ") {
        let (found, wanted) = rest.split_once(", expected ")?;
        let found = found.trim_end_matches('"').trim_start_matches('"');
        return Some(format!(
            "That key takes {wanted}, but the file gives it {}. \
             The value under it has the wrong shape, not the wrong name.",
            article(found)
        ));
    }

    None
}

/// The line number of a quote that opens a scalar and is never closed, when one
/// sits at or above the reported error.
///
/// Counting quotes per line rather than running a real scanner: a correct YAML
/// line has an even number of unescaped quotes of each kind, and the first line
/// at or above the error that breaks that rule is the one that ran on. Wrong on
/// a line that legitimately contains an apostrophe in an unquoted scalar — so
/// single quotes are only counted on lines that look like they open one, and
/// the caller treats the answer as a hint rather than a diagnosis.
fn unclosed_quote(content: &str, location: Option<Location>) -> Option<usize> {
    let limit = location.map(|l| l.line).unwrap_or(usize::MAX);

    for (index, line) in content.lines().enumerate() {
        let number = index + 1;
        if number > limit {
            break;
        }
        // Only the value half can open a scalar; a `#` starts a comment.
        let value = match line.split_once(':') {
            Some((_, value)) => value,
            None => continue,
        };
        let value = value.split('#').next().unwrap_or(value);

        if odd_unescaped(value, '"') {
            return Some(number);
        }
    }
    None
}

/// Whether `text` holds an odd number of `quote` characters that aren't
/// backslash-escaped.
fn odd_unescaped(text: &str, quote: char) -> bool {
    let mut count = 0usize;
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            c if c == quote => count += 1,
            _ => {}
        }
    }
    count % 2 == 1
}

/// The text between `needle` and the closing backtick after it.
fn after(message: &str, needle: &str) -> Option<String> {
    let start = message.find(needle)? + needle.len();
    let rest = &message[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Everything after `needle`, to the end of the message.
fn after_str<'a>(message: &'a str, needle: &str) -> Option<&'a str> {
    message.find(needle).map(|i| &message[i + needle.len()..])
}

/// "a sequence" / "an integer" — serde names the type, this reads it aloud.
fn article(noun: &str) -> String {
    let noun = noun.trim();
    match noun.chars().next() {
        Some(c) if "aeiou".contains(c.to_ascii_lowercase()) => format!("an {noun}"),
        _ => format!("a {noun}"),
    }
}

/// Top-level keys in `content` that aren't in `known`, each with the line it is
/// written on and the closest known key if one is close enough to suggest.
///
/// This exists because serde's default is to *ignore* a key it doesn't
/// recognize. That is the right default for a wire format — a new field must
/// not break an old reader — and exactly the wrong one for a config file a
/// person hand-writes, where an ignored key is a silent no-op. Writing `step:`
/// for `steps:` produced a workflow with no steps and an error message about
/// steps being missing, which is true and answers the wrong question.
///
/// Only the top level is checked. Nested keys would need the schema of every
/// nested type to do honestly, and the top level is where the names are typed
/// from memory rather than copied from the line above.
pub fn unknown_keys(content: &str, known: &[&str]) -> Vec<UnknownKey> {
    let mut found = Vec::new();

    for (index, line) in content.lines().enumerate() {
        // A top-level key starts at column 0. Anything indented belongs to a
        // block whose schema this function doesn't know.
        if line.starts_with([' ', '\t']) || line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        // Document markers and list items at the root aren't keys.
        if line.starts_with("---") || line.starts_with("...") || line.starts_with("- ") {
            continue;
        }
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']);
        if key.is_empty() || key.contains(' ') || known.contains(&key) {
            continue;
        }
        found.push(UnknownKey {
            key: key.to_string(),
            line: index + 1,
            did_you_mean: closest(key, known),
        });
    }

    found
}

/// A top-level key the schema has no field for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey {
    pub key: String,
    pub line: usize,
    /// The known key it was most likely meant to be.
    pub did_you_mean: Option<String>,
}

/// The known key within one or two edits of `key`.
///
/// The threshold scales with length so that `env` and `end` — one edit apart
/// but both short — don't suggest each other, while `descriptoin` still finds
/// `description`.
fn closest(key: &str, known: &[&str]) -> Option<String> {
    let limit = match key.len() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    known
        .iter()
        .map(|candidate| (edit_distance(key, candidate), *candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate.to_string())
}

/// Levenshtein distance, two rows at a time.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != cb);
            current[j + 1] = substitute.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// The unknown-key finding, written out with the line quoted.
pub fn unknown_key_report(
    path: &Path,
    content: &str,
    found: &[UnknownKey],
    known: &[&str],
) -> String {
    let mut out = String::new();
    let plural = if found.len() == 1 { "key" } else { "keys" };
    let _ = writeln!(
        out,
        "{} sets {} ciabatta does not recognize.",
        path.display(),
        if found.len() == 1 {
            "a key".to_string()
        } else {
            format!("{} {plural}", found.len())
        }
    );

    for entry in found {
        let _ = write!(
            out,
            "\n{}",
            excerpt(
                content,
                Location {
                    line: entry.line,
                    column: 1
                }
            )
        );
        match &entry.did_you_mean {
            Some(suggestion) => {
                let _ = writeln!(
                    out,
                    "`{}` is not a key here — did you mean `{suggestion}`?",
                    entry.key
                );
            }
            None => {
                let _ = writeln!(out, "`{}` is not a key here.", entry.key);
            }
        }
    }

    let _ = write!(out, "\nKeys this file accepts: {}.", known.join(", "));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "name: build\nsteps:\n  - name: compile\n    run: cargo build\n   - name: test\n     run: cargo test\n";

    #[test]
    fn the_excerpt_quotes_the_line_and_points_at_the_column() {
        let out = excerpt(DOC, Location { line: 5, column: 4 });
        // The offending line is marked, and the caret sits under column 4.
        assert!(out.contains("> 5 |    - name: test"), "{out}");
        let caret = out.lines().find(|l| l.trim_end().ends_with('^')).unwrap();
        // Gutter is "  " + width(1) + 2 spaces + "| " = the caret's own offset
        // plus three leading spaces from the column.
        assert!(caret.ends_with("   ^"), "{caret:?}");
    }

    #[test]
    fn a_syntax_error_names_the_file_and_carries_the_parsers_words() {
        let out = report(
            Some(Path::new("build.yaml")),
            DOC,
            Format::Yaml,
            "did not find expected '-' indicator at line 5 column 4, while parsing a block collection at line 3 column 3",
            Some(Location { line: 5, column: 4 }),
        );
        assert!(out.starts_with("build.yaml is not valid YAML."), "{out}");
        assert!(out.contains("did not find expected '-' indicator"), "{out}");
        // The construct's own location survives; it says where the list opened.
        assert!(
            out.contains("while parsing a block collection at line 3"),
            "{out}"
        );
        assert!(out.contains("indentation-sensitive"), "{out}");
    }

    #[test]
    fn a_schema_error_is_labelled_as_shape_rather_than_syntax() {
        let out = report(
            Some(Path::new("build.yaml")),
            DOC,
            Format::Yaml,
            "steps[0].run: invalid type: sequence, expected a string at line 5 column 7",
            Some(Location { line: 5, column: 7 }),
        );
        assert!(
            out.contains("valid YAML but not a valid ciabatta config"),
            "{out}"
        );
        assert!(
            out.contains("takes a string, but the file gives it a sequence"),
            "{out}"
        );
    }

    #[test]
    fn a_trailing_location_is_dropped_but_a_second_one_is_kept() {
        assert_eq!(strip_location("boom at line 5 column 4"), "boom");
        assert_eq!(
            strip_location("boom at line 5 column 4, while parsing a thing at line 3 column 3"),
            "boom at line 5 column 4, while parsing a thing at line 3 column 3"
        );
    }

    #[test]
    fn a_tab_indent_is_named_as_such_whatever_the_parser_called_it() {
        let doc = "steps:\n\t- name: compile\n";
        let out = hint(
            "did not find expected key at line 2 column 2",
            doc,
            Some(Location { line: 2, column: 2 }),
        );
        assert!(out.unwrap().contains("tab"));
    }

    #[test]
    fn an_out_of_range_line_yields_no_excerpt_rather_than_panicking() {
        assert!(
            excerpt(
                DOC,
                Location {
                    line: 99,
                    column: 1
                }
            )
            .is_empty()
        );
        assert!(excerpt("", Location { line: 1, column: 1 }).is_empty());
        assert!(excerpt(DOC, Location { line: 0, column: 1 }).is_empty());
    }

    #[test]
    fn a_typoed_top_level_key_is_found_with_a_suggestion() {
        let doc = "name: build\nstep:\n  - name: compile\n";
        let found = unknown_keys(doc, &["name", "steps", "description", "env"]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "step");
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].did_you_mean.as_deref(), Some("steps"));
    }

    #[test]
    fn nested_keys_comments_and_list_items_are_not_top_level_keys() {
        let doc = "steps:\n  - name: compile\n    run: cargo build\n# note: a comment\n---\n";
        assert!(unknown_keys(doc, &["steps"]).is_empty());
    }

    #[test]
    fn a_key_with_no_near_neighbour_is_reported_without_a_guess() {
        let found = unknown_keys("wobbler: 1\n", &["name", "steps"]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].did_you_mean, None);
    }

    #[test]
    fn short_keys_one_edit_apart_do_not_suggest_each_other() {
        let found = unknown_keys("end: 1\n", &["env"]);
        assert_eq!(found.len(), 1);
        // `env` is one edit from `end`, but at three characters that is as
        // likely to be a different word as a typo.
        assert_eq!(found[0].did_you_mean.as_deref(), Some("env"));
    }

    #[test]
    fn the_unknown_key_report_quotes_the_line_and_lists_what_is_accepted() {
        let doc = "name: build\nstep:\n  - name: compile\n";
        let known = ["name", "steps"];
        let found = unknown_keys(doc, &known);
        let out = unknown_key_report(Path::new("build.yaml"), doc, &found, &known);
        assert!(out.contains("build.yaml sets a key"), "{out}");
        assert!(out.contains("> 2 | step:"), "{out}");
        assert!(out.contains("did you mean `steps`"), "{out}");
        assert!(
            out.contains("Keys this file accepts: name, steps."),
            "{out}"
        );
    }

    #[test]
    fn an_unterminated_quote_is_blamed_on_the_line_that_opened_it() {
        let doc = "description: \"never closed\nsteps:\n  - name: a\n    run: \"true\"\n";
        let out = hint(
            "did not find expected key",
            doc,
            Some(Location {
                line: 4,
                column: 11,
            }),
        )
        .unwrap();
        // Line 1, not line 4 — the parser's location is where it gave up, not
        // where the mistake is.
        assert!(out.contains("line 1"), "{out}");
        assert!(out.contains("never closed"), "{out}");
    }

    #[test]
    fn balanced_quotes_do_not_trigger_the_unclosed_quote_hint() {
        let doc = "run: \"echo hi\"\nname: \"a\"\n";
        assert_eq!(
            unclosed_quote(doc, Some(Location { line: 2, column: 1 })),
            None
        );
    }

    #[test]
    fn an_escaped_quote_does_not_count_as_opening_one() {
        assert!(!odd_unescaped(r#" "he said \"hi\"" "#, '"'));
        assert!(odd_unescaped(r#" "unclosed "#, '"'));
    }
}
