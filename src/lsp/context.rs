//! Where the cursor is inside a YAML document, resolved without parsing it.
//!
//! A parser is the right tool for a file on disk and the wrong one for a file
//! being typed into. The moment someone types `needs:` and presses enter the
//! document is invalid YAML — and that is exactly the keystroke at which
//! completions have to work. So this walks lines and indentation instead,
//! which degrades into "I don't know" rather than into a parse error.
//!
//! It understands the subset ciabatta's config actually uses: block mappings,
//! block sequences, and inline flow sequences. Anything else resolves to
//! `None`, which suppresses completions rather than offering wrong ones.

/// What one physical line looks like, structurally.
#[derive(Debug, Clone, Default)]
struct Line {
    /// The column the line's *content* starts at, past any `- ` marker. This
    /// is the number that decides nesting: `- name: x` holds a key at the same
    /// depth as a plain `name: x` two columns further in.
    content_col: usize,
    /// Whether the line opens a block-sequence entry.
    is_item: bool,
    /// The mapping key on this line, if it has one.
    key: Option<String>,
    /// Everything after `key:`, trimmed.
    value: String,
    /// Blank or comment-only: carries no structure.
    skip: bool,
}

/// Where the cursor sits, and what it is in the middle of typing.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    /// Mapping keys from the document root down to the cursor's container,
    /// sequence indices omitted: `["steps", "needs"]` for a `needs` entry in
    /// some step, whichever step it happens to be.
    pub path: Vec<String>,
    /// Completing a mapping key rather than a value.
    pub in_key: bool,
    /// The word typed so far, for the client to filter on.
    pub word: String,
    /// Line of the `- ` opening the enclosing sequence entry, if there is one.
    pub item_line: Option<usize>,
}

impl Cursor {
    /// Whether the cursor's path is exactly these keys.
    pub fn at(&self, keys: &[&str]) -> bool {
        self.path.len() == keys.len() && self.path.iter().zip(keys).all(|(a, b)| a == b)
    }

    /// Whether the cursor's path ends with these keys — for fields that mean
    /// the same thing at more than one depth, like `cache.inputs`.
    pub fn ends_with(&self, keys: &[&str]) -> bool {
        self.path.len() >= keys.len()
            && self.path[self.path.len() - keys.len()..]
                .iter()
                .zip(keys)
                .all(|(a, b)| a == b)
    }
}

/// Split a line into its structural parts.
fn inspect(raw: &str) -> Line {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Line {
            skip: true,
            ..Line::default()
        };
    }

    let indent = raw.len() - raw.trim_start().len();
    // A `- ` marker shifts the content column without adding a mapping level.
    let (content_col, is_item, rest) = match raw[indent..].strip_prefix('-') {
        Some(after) => {
            let spaces = after.len() - after.trim_start().len();
            // `-x` is a scalar starting with a dash, not a sequence marker.
            if after.is_empty() || spaces > 0 {
                (indent + 1 + spaces.max(1), true, after.trim_start())
            } else {
                (indent, false, &raw[indent..])
            }
        }
        None => (indent, false, &raw[indent..]),
    };

    // `key:` or `key: value` — the key must look like an identifier, which is
    // true of every field in ciabatta's schema and false of most prose.
    if let Some(colon) = find_key_colon(rest) {
        let name = &rest[..colon];
        if !name.is_empty() && is_identifier(name) {
            return Line {
                content_col,
                is_item,
                key: Some(name.to_string()),
                value: rest[colon + 1..].trim().to_string(),
                skip: false,
            };
        }
    }

    Line {
        content_col,
        is_item,
        key: None,
        value: rest.trim().to_string(),
        skip: false,
    }
}

/// The colon that ends a mapping key: the first one at the top level, not one
/// inside a quoted scalar or a `{CIABATTA_*}` placeholder.
fn find_key_colon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match (quote, b) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"') | (None, b'\'') => quote = Some(b),
            (None, b':') => return Some(i),
            (None, _) => {}
        }
    }
    None
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The last identifier-ish run before the cursor — what has been typed so far,
/// and what a completion replaces.
///
/// `:` belongs to it, because `proto:generate` is one reference rather than
/// two words. `/` does not: a `publish_path` is a path, and completing a
/// `{CIABATTA_*}` placeholder in it must replace the placeholder alone.
fn word_before(text: &str) -> &str {
    let end = text.len();
    let start = text
        .char_indices()
        .rev()
        .find(|(_, c)| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' | ':' | '-' | '{' | '}'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    &text[start..end]
}

/// Strip the quotes a half-typed value may be wrapped in.
fn unquote(s: &str) -> &str {
    s.trim_matches(['"', '\''])
}

/// Whether an unclosed `[` puts the cursor inside a flow sequence.
fn in_flow_sequence(before: &str) -> bool {
    match before.rfind('[') {
        Some(open) => !before[open..].contains(']'),
        None => false,
    }
}

/// Resolve the cursor's position in the document's structure.
///
/// `line` and `character` are zero-based, as the protocol reports them.
pub fn resolve(lines: &[&str], line: usize, character: usize) -> Option<Cursor> {
    let raw = *lines.get(line)?;
    // `character` is a UTF-16 offset in the protocol; ciabatta's config is
    // overwhelmingly ASCII, and clamping is what keeps a stray emoji in a
    // description from panicking the server.
    let cut = raw
        .char_indices()
        .nth(character)
        .map(|(i, _)| i)
        .unwrap_or(raw.len());
    let before = &raw[..cut];

    // Comments are prose; there is nothing to complete inside one.
    if before.contains('#') {
        return None;
    }

    let self_line = inspect(if before.trim().is_empty() {
        raw
    } else {
        before
    });
    let flow = in_flow_sequence(before);

    let mut path: Vec<String> = Vec::new();
    let in_key;
    let word;
    let mut search_col;

    if let Some(key) = self_line
        .key
        .clone()
        .filter(|_| before.contains(':') || flow)
    {
        // `key: value|` — completing this key's value.
        path.push(key);
        in_key = false;
        word = unquote(word_before(before)).to_string();
        search_col = self_line.content_col;
    } else if self_line.is_item && self_line.key.is_none() {
        // `- value|` — an entry in a sequence of scalars.
        in_key = false;
        word = unquote(word_before(before)).to_string();
        search_col = self_line.content_col;
    } else {
        // A bare word at some indentation: a mapping key being typed. Its own
        // column is where the cursor is, less what has been typed of it.
        in_key = true;
        word = word_before(before).to_string();
        search_col = character.saturating_sub(word.chars().count());
    }

    // Walk outwards. Every line indented strictly less than where we are is
    // the next container up; same-level lines are siblings.
    let mut item_line: Option<usize> = None;
    for i in (0..line).rev() {
        let info = inspect(lines[i]);
        if info.skip {
            continue;
        }
        if info.content_col >= search_col {
            // A sibling. If it opens a sequence entry at our own level, that
            // entry is the mapping we are inside of.
            if info.is_item && info.content_col == search_col && item_line.is_none() {
                item_line = Some(i);
            }
            continue;
        }
        if info.is_item && item_line.is_none() {
            item_line = Some(i);
        }
        let at_root = info.content_col == 0;
        if let Some(key) = info.key {
            path.insert(0, key);
            if at_root {
                break;
            }
        }
        search_col = info.content_col;
    }

    Some(Cursor {
        path,
        in_key,
        word,
        item_line,
    })
}

/// The `name:` of the block-sequence entry the cursor is inside — the step
/// being edited, so its own name can be kept out of its `needs:` list.
pub fn item_name(lines: &[&str], item_line: Option<usize>) -> Option<String> {
    let start = inspect(lines[item_line?]);
    for (i, raw) in lines.iter().enumerate().skip(item_line?) {
        let info = inspect(raw);
        if info.skip {
            continue;
        }
        if i > item_line? && (info.content_col < start.content_col || info.is_item) {
            break; // left this entry
        }
        if info.content_col == start.content_col && info.key.as_deref() == Some("name") {
            let name = unquote(&info.value);
            return (!name.is_empty()).then(|| name.to_string());
        }
    }
    None
}

/// Every step name in the document's top-level `steps:` list, in file order.
///
/// Read from the buffer rather than from the parsed workflow so it stays right
/// while the file is being edited, which is the only time it is asked for.
pub fn step_names(lines: &[&str]) -> Vec<String> {
    let mut names = Vec::new();
    let mut entry_col: Option<usize> = None;
    let mut in_steps = false;

    for raw in lines {
        let info = inspect(raw);
        if info.skip {
            continue;
        }
        if !in_steps {
            in_steps = info.key.as_deref() == Some("steps") && info.content_col == 0;
            continue;
        }
        match entry_col {
            // The first entry fixes the column every later one lines up with.
            None if info.is_item => entry_col = Some(info.content_col),
            None => break,
            Some(col) if info.content_col < col => break,
            Some(_) => {}
        }
        if Some(info.content_col) == entry_col
            && info.key.as_deref() == Some("name")
            && !info.value.is_empty()
        {
            names.push(unquote(&info.value).to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split a fixture on newlines and put the cursor at the `|` marker.
    fn at(src: &str) -> Cursor {
        let raw: Vec<String> = src.lines().map(str::to_string).collect();
        let (line, character) = raw
            .iter()
            .enumerate()
            .find_map(|(i, l)| l.find('|').map(|c| (i, c)))
            .expect("fixture needs a | cursor marker");
        let cleaned: Vec<String> = raw.iter().map(|l| l.replace('|', "")).collect();
        let refs: Vec<&str> = cleaned.iter().map(String::as_str).collect();
        resolve(&refs, line, character).expect("cursor should resolve")
    }

    #[test]
    fn a_bare_word_at_the_root_is_a_top_level_key() {
        let c = at("desc|");
        assert!(c.in_key);
        assert!(c.path.is_empty());
        assert_eq!(c.word, "desc");
    }

    #[test]
    fn a_key_being_typed_inside_a_step_knows_it_is_in_steps() {
        let c = at("steps:\n  - name: build\n    tim|");
        assert!(c.in_key);
        assert!(c.at(&["steps"]));
        assert_eq!(c.word, "tim");
    }

    #[test]
    fn a_sequence_entry_under_a_step_key_resolves_through_both() {
        let c = at("steps:\n  - name: unit\n    needs:\n      - for|");
        assert!(!c.in_key);
        assert!(c.at(&["steps", "needs"]));
        assert_eq!(c.word, "for");
        assert_eq!(
            item_name(&["steps:", "  - name: unit"], c.item_line),
            Some("unit".into())
        );
    }

    #[test]
    fn a_workflow_level_needs_is_not_a_step_level_one() {
        let c = at("needs:\n  - too|");
        assert!(c.at(&["needs"]));
    }

    #[test]
    fn an_inline_flow_sequence_is_a_value_position() {
        let c = at("steps:\n  - name: a\n    requires: [car|");
        assert!(!c.in_key);
        assert!(c.at(&["steps", "requires"]));
        assert_eq!(c.word, "car");
    }

    #[test]
    fn nested_maps_carry_their_arbitrary_key_in_the_path() {
        let c = at("registries:\n  nexus:\n    ur|");
        assert!(c.in_key);
        assert!(c.at(&["registries", "nexus"]));
    }

    #[test]
    fn a_value_on_the_same_line_as_its_key_resolves_to_that_key() {
        let c = at("workspace:\n  owner: Hen|");
        assert!(!c.in_key);
        assert!(c.at(&["workspace", "owner"]));
    }

    #[test]
    fn a_path_segment_is_replaceable_on_its_own() {
        let c = at("steps:\n  - name: p\n    publish_path: app/{CIABATTA_|");
        assert!(c.at(&["steps", "publish_path"]));
        assert_eq!(c.word, "{CIABATTA_");
    }

    #[test]
    fn comments_offer_nothing() {
        let lines = ["# just some prose here"];
        assert!(resolve(&lines, 0, 12).is_none());
    }

    #[test]
    fn step_names_are_found_wherever_name_sits_in_the_entry() {
        let lines = [
            "steps:",
            "  - name: format",
            "    run: cargo fmt",
            "  - description: lint it",
            "    name: lint",
            "cache:",
            "  inputs:",
            "    - name: not-a-step",
        ];
        assert_eq!(step_names(&lines), vec!["format", "lint"]);
    }

    #[test]
    fn a_dash_in_a_value_is_not_a_sequence_marker() {
        let c = at("steps:\n  - name: a\n    run: cargo test -- --nocapture\n    des|");
        assert!(c.at(&["steps"]));
        assert!(c.in_key);
    }
}
