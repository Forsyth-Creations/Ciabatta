//! Turning a cursor position into a list of things that would actually work.
//!
//! Deliberately narrow: this offers **values**, never field names. The JSON
//! Schema the editor extensions register already knows the shape of the file —
//! which keys exist, what type each takes, what it's for — and it knows it in
//! one place. What a schema cannot know is what *this repository* contains, and
//! that is the whole of what's below: which sub-workspaces exist, which
//! workflows they define, which tools the root's `toolchain:` promises to
//! install, which registries are configured, which tags the repo already uses.
//!
//! Those are exactly the fields people get wrong, because getting them right
//! means remembering something that lives in another package's file.

use serde_json::{Value, json};

use super::context::{Cursor, item_name, step_names};
use super::index::{Index, Role};

/// How a completion item sorts and what kind of icon it gets. The protocol's
/// numbering; only the few kinds this server emits are named.
mod kind {
    pub const MODULE: i64 = 9;
    pub const PROPERTY: i64 = 10;
    pub const ENUM: i64 = 13;
    pub const KEYWORD: i64 = 14;
    pub const CONSTANT: i64 = 21;
    pub const EVENT: i64 = 23;
    pub const VARIABLE: i64 = 6;
}

/// One suggestion, before it becomes protocol JSON.
struct Item {
    label: String,
    /// The grey text after the label: why this is the right choice.
    detail: String,
    kind: i64,
}

impl Item {
    fn new(label: impl Into<String>, detail: impl Into<String>, kind: i64) -> Self {
        Item {
            label: label.into(),
            detail: detail.into(),
            kind,
        }
    }
}

/// The phases `kind:` recognises. Only `push` and `pull` change behaviour —
/// the rest label the node on the graph — but offering the set is how a repo
/// ends up using one vocabulary instead of six.
const PHASES: &[(&str, &str)] = &[
    (
        "setup",
        "Preparation: toolchains, credentials, generated sources",
    ),
    ("build", "Compiles or bundles something"),
    ("test", "Verifies something"),
    ("deploy", "Releases something"),
    (
        "push",
        "Publishes an artifact to `registry:` — a built-in action",
    ),
    (
        "pull",
        "Fetches an artifact from `registry:` — a built-in action",
    ),
];

/// The variables ciabatta substitutes into `publish_path`, filled in from CI or
/// from local git with `--local`.
const SUBSTITUTIONS: &[(&str, &str)] = &[
    ("{CIABATTA_BRANCH}", "The branch being built"),
    ("{CIABATTA_COMMIT}", "The commit SHA being built"),
    ("{CIABATTA_TAG}", "The tag being built, when there is one"),
    ("{CIABATTA_BUILD_NUMBER}", "The CI run or build number"),
    ("{CIABATTA_PATH}", "Where a glob-list publish_path lands"),
];

/// Every suggestion for this cursor, as protocol `CompletionItem`s.
///
/// `lines` is the live buffer — step names come from what has been typed, not
/// from what was last saved, because the step you want to depend on is often
/// the one you added a moment ago. `line` and `character` are the cursor, used
/// to spell out the range each item replaces.
pub fn items(
    cursor: &Cursor,
    role: &Role,
    member: Option<&str>,
    index: &Index,
    lines: &[&str],
    line: usize,
    character: usize,
) -> Value {
    // Field names belong to the JSON Schema, which has the documentation for
    // them. Two sources for one list would only disagree.
    if cursor.in_key {
        return json!([]);
    }

    let items = match role {
        Role::Workflow(_) => in_workflow(cursor, member, index, lines),
        Role::Config => in_config(cursor, member, index),
    };

    // Replace exactly what has been typed, rather than leaving the client to
    // infer the range from its own idea of a word. `{CIABATTA_` is one word to
    // us and three to most editors, and the difference shows up as a mangled
    // insertion.
    let range = json!({
        "start": { "line": line, "character": character.saturating_sub(cursor.word.chars().count()) },
        "end":   { "line": line, "character": character },
    });

    let items: Vec<Value> = items
        .into_iter()
        .filter(|item| matches_word(&item.label, &cursor.word))
        .enumerate()
        .map(|(i, item)| {
            json!({
                "label": item.label,
                "detail": item.detail,
                "kind": item.kind,
                "textEdit": { "range": range, "newText": item.label },
                // Our suggestions are drawn from the repository itself, so they
                // should sit above whatever the schema offers for the same
                // position rather than being interleaved alphabetically.
                "sortText": format!("0{i:04}"),
            })
        })
        .collect();

    Value::Array(items)
}

/// Case-insensitive prefix or substring match, the filtering an editor does
/// anyway — repeated here so a client that doesn't filter still behaves.
fn matches_word(label: &str, word: &str) -> bool {
    if word.is_empty() {
        return true;
    }
    label.to_lowercase().contains(&word.to_lowercase())
}

/// Suggestions inside `.ciabatta/workflows/<name>.yaml`.
fn in_workflow(cursor: &Cursor, member: Option<&str>, index: &Index, lines: &[&str]) -> Vec<Item> {
    // A step's `needs:` names steps in this same file. A workflow's `needs:`
    // names other packages' workflows. Same word, different vocabulary — and
    // mixing them up is the mistake this exists to prevent.
    if cursor.at(&["steps", "needs"])
        || cursor.at(&["steps", "on_error"])
        || cursor.at(&["steps", "retry"])
    {
        let own = item_name(lines, cursor.item_line);
        return step_names(lines)
            .into_iter()
            .filter(|name| Some(name.as_str()) != own.as_deref())
            .map(|name| Item::new(name, "a step in this workflow", kind::EVENT))
            .collect();
    }

    if cursor.at(&["needs"]) {
        return workflow_refs(index, member);
    }

    if cursor.ends_with(&["requires"]) {
        return tools(index);
    }
    if cursor.ends_with(&["tags"]) {
        return tags(index);
    }
    if cursor.at(&["steps", "registry"]) {
        return registries(index);
    }
    if cursor.at(&["steps", "kind"]) {
        return PHASES
            .iter()
            .map(|(name, why)| Item::new(*name, *why, kind::ENUM))
            .collect();
    }
    if cursor.at(&["steps", "publish_path"]) {
        return SUBSTITUTIONS
            .iter()
            .map(|(name, why)| Item::new(*name, *why, kind::CONSTANT))
            .collect();
    }
    if cursor.at(&["steps", "when"]) || cursor.at(&["steps", "skip_if"]) {
        return env_vars(index, "compared against this step's condition");
    }
    if cursor.at(&["REQUIRED_ENV"]) || cursor.ends_with(&["cache", "env"]) {
        return env_vars(index, "used elsewhere in this repository");
    }
    if cursor.at(&["steps", "from"]) {
        return step_names(lines)
            .into_iter()
            .map(|name| Item::new(name, "the push step this pull mirrors", kind::EVENT))
            .collect();
    }

    Vec::new()
}

/// Suggestions inside `.ciabatta/ciabatta.yaml`.
fn in_config(cursor: &Cursor, member: Option<&str>, index: &Index) -> Vec<Item> {
    if cursor.at(&["workspace", "depends_on"]) {
        return workflow_refs(index, member);
    }
    if cursor.ends_with(&["requires"]) {
        return tools(index);
    }
    if cursor.ends_with(&["tags"]) {
        return tags(index);
    }
    if cursor.ends_with(&["cache", "env"]) {
        return env_vars(index, "used elsewhere in this repository");
    }

    // Inline workflows (`workflows.<name>:`) are workflows, and the arbitrary
    // name in the middle of the path is not part of the shape.
    if cursor.path.first().is_some_and(|k| k == "workflows") && cursor.path.len() > 2 {
        let mut inner = cursor.clone();
        inner.path.drain(0..2);
        return in_workflow(&inner, member, index, &[]);
    }

    Vec::new()
}

fn workflow_refs(index: &Index, member: Option<&str>) -> Vec<Item> {
    index
        .workflow_refs(member)
        .into_iter()
        .map(|(reference, detail)| Item::new(reference, detail, kind::MODULE))
        .collect()
}

fn tools(index: &Index) -> Vec<Item> {
    index
        .tools
        .iter()
        .map(|(tool, description)| {
            Item::new(
                tool,
                description
                    .clone()
                    .unwrap_or_else(|| "declared in the root's toolchain:".into()),
                kind::KEYWORD,
            )
        })
        .collect()
}

fn tags(index: &Index) -> Vec<Item> {
    index
        .tags
        .iter()
        .map(|tag| Item::new(tag, "already used in this repository", kind::PROPERTY))
        .collect()
}

fn registries(index: &Index) -> Vec<Item> {
    index
        .registries
        .iter()
        .map(|(name, url)| Item::new(name, url, kind::MODULE))
        .collect()
}

fn env_vars(index: &Index, detail: &str) -> Vec<Item> {
    index
        .env
        .iter()
        .map(|name| Item::new(name, detail, kind::VARIABLE))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::context::resolve;
    use crate::lsp::index::MemberInfo;

    fn sample_index() -> Index {
        Index {
            members: vec![
                MemberInfo {
                    name: "api".into(),
                    description: Some("The public REST API".into()),
                    owner: "Henry Forsyth".into(),
                    workflows: [("build".to_string(), None)].into_iter().collect(),
                },
                MemberInfo {
                    name: "proto".into(),
                    description: None,
                    owner: "Henry Forsyth".into(),
                    workflows: [("generate".to_string(), Some("Generate protobufs".into()))]
                        .into_iter()
                        .collect(),
                },
            ],
            tools: [("cargo".to_string(), Some("The Rust toolchain".into()))]
                .into_iter()
                .collect(),
            registries: [("nexus".to_string(), "http://localhost:8527".to_string())]
                .into_iter()
                .collect(),
            tags: ["fast".to_string(), "slow".to_string()]
                .into_iter()
                .collect(),
            env: ["API_TOKEN".to_string()].into_iter().collect(),
        }
    }

    fn complete(src: &str, role: Role, member: &str) -> Vec<String> {
        let raw: Vec<String> = src.lines().map(str::to_string).collect();
        let (line, character) = raw
            .iter()
            .enumerate()
            .find_map(|(i, l)| l.find('|').map(|c| (i, c)))
            .expect("fixture needs a | cursor marker");
        let cleaned: Vec<String> = raw.iter().map(|l| l.replace('|', "")).collect();
        let refs: Vec<&str> = cleaned.iter().map(String::as_str).collect();
        let cursor = resolve(&refs, line, character).expect("cursor should resolve");
        let value = items(
            &cursor,
            &role,
            Some(member),
            &sample_index(),
            &refs,
            line,
            character,
        );
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_steps_needs_offers_the_other_steps_in_this_file() {
        let got = complete(
            "steps:\n  - name: format\n    run: cargo fmt\n  - name: lint\n    needs:\n      - |",
            Role::Workflow("test".into()),
            "api",
        );
        assert_eq!(got, vec!["format"]);
    }

    #[test]
    fn a_workflows_needs_offers_other_packages_not_steps() {
        let got = complete(
            "description: Build it\nneeds:\n  - |\nsteps:\n  - name: compile\n    run: make",
            Role::Workflow("build".into()),
            "api",
        );
        assert!(got.contains(&"proto:generate".to_string()));
        assert!(!got.contains(&"compile".to_string()));
        // A package never depends on itself.
        assert!(!got.iter().any(|r| r.starts_with("api")));
    }

    #[test]
    fn requires_offers_what_the_toolchain_promises_to_install() {
        let got = complete("requires:\n  - car|", Role::Workflow("build".into()), "api");
        assert_eq!(got, vec!["cargo"]);
    }

    #[test]
    fn a_push_steps_registry_offers_the_configured_ones() {
        let got = complete(
            "steps:\n  - name: publish\n    kind: push\n    registry: |",
            Role::Workflow("release".into()),
            "api",
        );
        assert_eq!(got, vec!["nexus"]);
    }

    #[test]
    fn publish_path_offers_the_substitution_variables() {
        let got = complete(
            "steps:\n  - name: publish\n    publish_path: app/{CIABATTA_|",
            Role::Workflow("release".into()),
            "api",
        );
        assert!(got.contains(&"{CIABATTA_COMMIT}".to_string()));
    }

    #[test]
    fn depends_on_in_the_config_offers_workflow_references() {
        let got = complete(
            "workspace:\n  name: api\n  depends_on:\n    - |",
            Role::Config,
            "api",
        );
        assert!(got.contains(&"proto:generate".to_string()));
    }

    #[test]
    fn field_names_are_left_to_the_json_schema() {
        let got = complete(
            "steps:\n  - name: a\n    desc|",
            Role::Workflow("t".into()),
            "api",
        );
        assert!(got.is_empty());
    }

    #[test]
    fn a_position_with_nothing_repo_specific_to_say_says_nothing() {
        let got = complete("description: |", Role::Workflow("build".into()), "api");
        assert!(got.is_empty());
    }
}
