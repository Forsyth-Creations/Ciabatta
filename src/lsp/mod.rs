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
//! * [`runs`] — how the project's runs are going, live and last time.

mod complete;
mod context;
mod diagnostics;
mod index;
mod rpc;
mod runs;
mod schema;

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use index::{Cache, Location, Role, classify};
use runs::RunWatch;

/// How often the server looks at the project's runs. Often enough that a
/// progress bar moves while you watch; rarely enough to cost nothing.
const RUN_POLL: Duration = Duration::from_secs(2);

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
///
/// Messages are read on a thread of their own, so the loop can also wake on a
/// timer: run status has to reach the editor when a run moves, not only when
/// the user next types something.
pub fn serve() -> Result<()> {
    let (messages, inbox) = mpsc::channel::<Result<Option<rpc::Message>>>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = BufReader::new(stdin.lock());
        loop {
            let message = rpc::read(&mut input);
            let more = matches!(message, Ok(Some(_)));
            if messages.send(message).is_err() || !more {
                break;
            }
        }
    });

    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    let mut documents = Documents::new();
    let mut cache = Cache::default();
    let mut watch = RunWatch::new();
    let mut initialized = false;
    let mut shutting_down = false;
    let mut last_poll = Instant::now();

    loop {
        // Look at the runs when it's time, whether or not a message is waiting
        // — an editor busy sending keystrokes would otherwise starve it.
        if initialized && last_poll.elapsed() >= RUN_POLL {
            last_poll = Instant::now();
            if watch.tick(&mut output)? {
                // A run finished: the open workflows' "last run" is stale.
                for (uri, text) in &documents {
                    publish(&mut output, uri, text, &mut cache, &watch)?;
                }
            }
        }
        let message = match inbox.recv_timeout(RUN_POLL.saturating_sub(last_poll.elapsed())) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let Some(message) = message? else { break };

        match message.method.as_str() {
            "initialize" => {
                let id = message.id.expect("initialize is a request");
                watch.initialize(&message.params);
                rpc::respond(&mut output, &id, capabilities())?;
            }
            "initialized" => initialized = true,

            "textDocument/didOpen" => {
                let uri = uri_of(&message.params);
                let text = message.params["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if let Some(uri) = uri {
                    if let Some((path, _)) = locate(&uri) {
                        watch.adopt(&path);
                    }
                    publish(&mut output, &uri, &text, &mut cache, &watch)?;
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
                publish(&mut output, &uri, &text, &mut cache, &watch)?;
                documents.insert(uri, text);
            }
            "textDocument/didSave" => {
                // The saved file is now part of the repository other files
                // resolve against.
                cache.invalidate();
                if let Some(uri) = uri_of(&message.params)
                    && let Some(text) = documents.get(&uri).cloned()
                {
                    publish(&mut output, &uri, &text, &mut cache, &watch)?;
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
            "textDocument/hover" => {
                let id = message.id.expect("hover is a request");
                let result = hover(&message.params, &documents, &mut cache, &watch);
                rpc::respond(&mut output, &id, result)?;
            }

            "shutdown" => {
                shutting_down = true;
                let id = message.id.expect("shutdown is a request");
                rpc::respond(&mut output, &id, Value::Null)?;
            }
            "exit" => {
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
                // `{` opens a `{CIABATTA_*}` substitution; `[` and `,` start
                // the next entry of an inline list like `needs: [a, b]`.
                "triggerCharacters": ["-", " ", ":", "{", "[", ","],
            },
            // How a workflow's runs are going — see `runs`.
            "hoverProvider": true,
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

fn publish(
    output: &mut impl Write,
    uri: &str,
    text: &str,
    cache: &mut Cache,
    watch: &RunWatch,
) -> Result<()> {
    let diagnostics = match locate(uri) {
        Some((_, location)) => {
            let index = cache.get(&location.member_dir);
            let lines: Vec<&str> = text.lines().collect();
            let mut found = diagnostics::check(&lines, &location.role, &index);
            // A workflow whose last run failed says so on its first line.
            if let Role::Workflow(name) = &location.role
                && let Some(failed) =
                    watch.diagnostic(member_of(&location, &index).as_deref(), name)
                && let Some(list) = found.as_array_mut()
            {
                list.push(failed);
            }
            found
        }
        None => json!([]),
    };
    rpc::notify(
        output,
        "textDocument/publishDiagnostics",
        json!({ "uri": uri, "diagnostics": diagnostics }),
    )
}

/// What a workflow file's top-level lines say on hover: how its runs are going.
///
/// Only the top-level lines (`description:`, `steps:` …), which are about the
/// workflow as a whole; a hover on a step or a value would be answering a
/// question nobody asked there.
fn hover(params: &Value, documents: &Documents, cache: &mut Cache, watch: &RunWatch) -> Value {
    let Some(uri) = uri_of(params) else {
        return Value::Null;
    };
    let Some((_, location)) = locate(&uri) else {
        return Value::Null;
    };
    let Role::Workflow(name) = &location.role else {
        return Value::Null;
    };
    let line = params["position"]["line"].as_u64().unwrap_or(0) as usize;
    let top_level = documents
        .get(&uri)
        .and_then(|text| text.lines().nth(line))
        .is_some_and(|l| !l.is_empty() && !l.starts_with([' ', '\t', '#', '-']));
    if !top_level {
        return Value::Null;
    }
    let index = cache.get(&location.member_dir);
    match watch.hover(member_of(&location, &index).as_deref(), name) {
        Some(text) => json!({ "contents": { "kind": "markdown", "value": text } }),
        None => Value::Null,
    }
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
