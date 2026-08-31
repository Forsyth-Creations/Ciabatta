//! `ciabatta lsp` — the editor half of the tool.
//!
//! A monorepo's config files are full of references to things defined in other
//! packages' files: the workflow a `needs:` points at, the tool a `requires:`
//! expects the root to know how to install, the registry a `push` step
//! publishes through. Getting one wrong is easy, and the feedback arrives at
//! build time, in someone else's terminal.
//!
//! So the knowledge ciabatta already has — [`Workspace::load`] finds every
//! member and every workflow in the repo — is served to the editor over the
//! Language Server Protocol. One server, driven identically by the VS Code and
//! Zed extensions in `editors/`, which means a completion offered in either
//! editor is a reference the build will resolve.
//!
//! It deliberately does **not** describe the shape of the file. Which fields
//! exist, what each takes and what it's for lives in the JSON Schemas the
//! extensions register, which the editors' own YAML support already reads.
//! Two descriptions of one schema would only drift apart.
//!
//! * [`context`] — where the cursor is, resolved on half-typed YAML.
//! * [`index`] — the repository, cached between keystrokes.
//! * [`complete`] — cursor plus repository to a list of suggestions.
//! * [`diagnostics`] — the references that don't resolve.

mod complete;
mod context;
mod diagnostics;
mod index;
mod rpc;
mod schema;

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use index::{Cache, Location, classify};

/// Documents the client has open, by URI. The client owns their contents once
/// it opens them, so this is the only place to read from — the file on disk is
/// whatever was last saved, not what is being typed.
type Documents = HashMap<String, String>;

/// Run the server until the client disconnects.
///
/// Speaks over stdin/stdout, which is how every editor launches a language
/// server. Nothing else may write to stdout for the duration — a stray
/// `println!` would be read as a malformed message — so diagnostics about the
/// server itself go to stderr, where editors collect them into a log.
pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    let mut documents = Documents::new();
    let mut cache = Cache::default();
    let mut shutting_down = false;

    while let Some(message) = rpc::read(&mut input)? {
        match message.method.as_str() {
            "initialize" => {
                let id = message.id.expect("initialize is a request");
                rpc::respond(&mut output, &id, capabilities())?;
            }
            "initialized" => {}

            "textDocument/didOpen" => {
                let uri = uri_of(&message.params);
                let text = message.params["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if let Some(uri) = uri {
                    publish(&mut output, &uri, &text, &mut cache)?;
                    documents.insert(uri, text);
                }
            }
            "textDocument/didChange" => {
                // Full-sync only: `capabilities` asks for whole documents, so
                // the last content change is the entire file.
                let Some(uri) = uri_of(&message.params) else {
                    continue;
                };
                let Some(text) = message.params["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                else {
                    continue;
                };
                let text = text.to_string();
                publish(&mut output, &uri, &text, &mut cache)?;
                documents.insert(uri, text);
            }
            "textDocument/didSave" => {
                // The saved file is now part of the repository other files
                // resolve against.
                cache.invalidate();
                if let Some(uri) = uri_of(&message.params)
                    && let Some(text) = documents.get(&uri).cloned()
                {
                    publish(&mut output, &uri, &text, &mut cache)?;
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = uri_of(&message.params) {
                    documents.remove(&uri);
                    // Clear its diagnostics: a closed file's warnings should
                    // not linger in the problems panel.
                    rpc::notify(
                        &mut output,
                        "textDocument/publishDiagnostics",
                        json!({ "uri": uri, "diagnostics": [] }),
                    )?;
                }
            }
            "workspace/didChangeWatchedFiles" => cache.invalidate(),

            "textDocument/completion" => {
                let id = message.id.expect("completion is a request");
                let result = completion(&message.params, &documents, &mut cache);
                rpc::respond(&mut output, &id, result)?;
            }

            "shutdown" => {
                shutting_down = true;
                let id = message.id.expect("shutdown is a request");
                rpc::respond(&mut output, &id, Value::Null)?;
            }
            "exit" => {
                rpc::drain(&mut input);
                // A client that exits without shutting down first is telling us
                // something went wrong; the protocol asks us to say so.
                return if shutting_down {
                    Ok(())
                } else {
                    anyhow::bail!("editor sent `exit` without `shutdown`")
                };
            }

            // A request we don't implement still needs an answer, or the client
            // waits forever. Notifications can simply be dropped.
            _ if message.is_request() => {
                let id = message.id.expect("checked");
                rpc::respond_error(
                    &mut output,
                    &id,
                    rpc::METHOD_NOT_FOUND,
                    &format!("ciabatta lsp does not implement {}", message.method),
                )?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// What this server tells the client it can do.
///
/// Modest on purpose: completion and diagnostics for ciabatta's own files. The
/// editor's YAML support keeps everything else — formatting, folding, the
/// schema — and there is no reason to compete with it.
fn capabilities() -> Value {
    json!({
        "capabilities": {
            // 1 = full sync. These files are a few hundred lines at most, and
            // incremental sync would be bookkeeping for no gain.
            "textDocumentSync": { "openClose": true, "change": 1, "save": true },
            "completionProvider": {
                // `:` opens the `<member>:<workflow>` half of a reference;
                // `{` opens a `{CIABATTA_*}` substitution.
                "triggerCharacters": ["-", " ", ":", "{"],
            },
        },
        "serverInfo": { "name": "ciabatta", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn uri_of(params: &Value) -> Option<String> {
    params["textDocument"]["uri"].as_str().map(str::to_string)
}

/// The document's path and place in the monorepo, or `None` if it isn't one of
/// ciabatta's files.
fn locate(uri: &str) -> Option<(PathBuf, Location)> {
    let path = rpc::uri_to_path(uri)?;
    let location = classify(&path)?;
    Some((path, location))
}

/// The member name a file belongs to: what its own `.ciabatta/ciabatta.yaml`
/// declares, or the directory name it defaults to.
fn member_of(location: &Location, index: &index::Index) -> Option<String> {
    let dir = &location.member_dir;
    index
        .members
        .iter()
        .find(|m| dir.ends_with(&m.name) || dir.file_name().is_some_and(|n| n == m.name.as_str()))
        .map(|m| m.name.clone())
        .or_else(|| Some(dir.file_name()?.to_str()?.to_string()))
}

fn publish(output: &mut impl Write, uri: &str, text: &str, cache: &mut Cache) -> Result<()> {
    let diagnostics = match locate(uri) {
        Some((_, location)) => {
            let index = cache.get(&location.member_dir);
            let lines: Vec<&str> = text.lines().collect();
            diagnostics::check(&lines, &location.role, &index)
        }
        None => json!([]),
    };
    rpc::notify(
        output,
        "textDocument/publishDiagnostics",
        json!({ "uri": uri, "diagnostics": diagnostics }),
    )
}

fn completion(params: &Value, documents: &Documents, cache: &mut Cache) -> Value {
    let empty = json!({ "isIncomplete": false, "items": [] });

    let Some(uri) = uri_of(params) else {
        return empty;
    };
    let Some((_, location)) = locate(&uri) else {
        return empty;
    };
    let Some(text) = documents.get(&uri) else {
        return empty;
    };

    let line = params["position"]["line"].as_u64().unwrap_or(0) as usize;
    let character = params["position"]["character"].as_u64().unwrap_or(0) as usize;
    let lines: Vec<&str> = text.lines().collect();

    let Some(cursor) = context::resolve(&lines, line, character) else {
        return empty;
    };

    let index = cache.get(&location.member_dir);
    let member = member_of(&location, &index);
    let items = complete::items(
        &cursor,
        &location.role,
        member.as_deref(),
        &index,
        &lines,
        line,
        character,
    );

    // `isIncomplete: false` — this list is the whole answer for this position,
    // so the client may filter it as the user keeps typing rather than asking
    // again on every keystroke.
    json!({ "isIncomplete": false, "items": items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use index::Role;

    #[test]
    fn a_yaml_file_outside_a_ciabatta_dir_is_not_ours() {
        assert!(locate("file:///repo/docker-compose.yml").is_none());
    }

    #[test]
    fn a_workflow_file_locates_to_its_member_and_name() {
        let (path, location) =
            locate("file:///repo/api/.ciabatta/workflows/build.yaml").expect("should locate");
        assert_eq!(
            path,
            PathBuf::from("/repo/api/.ciabatta/workflows/build.yaml")
        );
        assert_eq!(location.role, Role::Workflow("build".into()));
        assert_eq!(location.member_dir, PathBuf::from("/repo/api"));
    }

    #[test]
    fn completion_on_an_unopened_document_is_empty_not_an_error() {
        let mut cache = Cache::default();
        let params = json!({
            "textDocument": { "uri": "file:///repo/api/.ciabatta/workflows/build.yaml" },
            "position": { "line": 0, "character": 0 },
        });
        let result = completion(&params, &Documents::new(), &mut cache);
        assert_eq!(result["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn the_member_name_falls_back_to_the_directory() {
        let location = Location {
            role: Role::Config,
            member_dir: PathBuf::from("/repo/api"),
        };
        assert_eq!(
            member_of(&location, &index::Index::default()),
            Some("api".to_string())
        );
    }
}
