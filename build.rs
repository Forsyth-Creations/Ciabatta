//! Makes sure the directories the daemon embeds exist before the crate is
//! compiled.
//!
//! Three of them, all produced by yarn and all gitignored, and `include_dir!`
//! needs each to be present at compile time:
//!
//! * `tool_frontend/dist` — the daemon's web app. Built by
//!   `yarn workspace ciabatta-tool-frontend build`. Rather than fail the build
//!   (which would mean nobody can `cargo build` without node installed), we
//!   drop in a placeholder page that tells the reader how to build the real
//!   thing.
//! * `remote-cache-web/dist` — the remote cache server's admin app, built by
//!   `yarn workspace ciabatta-remote-cache-web build`. Same placeholder, same
//!   reasoning.
//! * `editors/dist` — the packaged editor extensions, built by
//!   `yarn workspace ciabatta-vscode build`. Here an empty directory is the
//!   honest answer
//!   rather than a placeholder: a `.vsix` that isn't an extension is worse
//!   than no `.vsix`, so the daemon serves whatever it finds and the web app
//!   lists only that, falling back to the releases page when it finds nothing.
//!
//! CI and the release workflow run the yarn builds *before* cargo, so shipped
//! binaries always carry the real app and the real extension. See the "Release
//! check" in the README.

use std::fs;
use std::path::Path;

/// Marker text also used by the release check to detect a placeholder build.
const PLACEHOLDER_MARKER: &str = "ciabatta-placeholder-bundle";

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let root = Path::new(&manifest_dir);

    editors_dist(&root.join("editors").join("dist"));
    web_dist(
        root,
        "tool_frontend",
        "ciabatta-tool-frontend",
        "the daemon",
    );
    web_dist(
        root,
        "remote-cache-web",
        "ciabatta-remote-cache-web",
        "the remote cache server",
    );
    protos(root);
}

/// Compile `proto/` into Rust types (prost, with pbjson for JSON) and the
/// matching TypeScript, all in `OUT_DIR`.
///
/// `protox` rather than `protoc`, so building ciabatta needs no protobuf
/// compiler installed. The TypeScript isn't written into the source tree from
/// here — a build script that edits checked-in files is a surprise — but a test
/// compares it with `remote-cache-web/src/gen/oven.ts` and rewrites that file
/// when asked (see `src/oven/proto.rs`).
fn protos(root: &Path) {
    println!("cargo:rerun-if-changed=proto");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set"));
    let fds = protox::compile(["oven.proto"], [root.join("proto")])
        .unwrap_or_else(|e| panic!("proto/oven.proto doesn't compile: {e:?}"));
    let encoded = prost::Message::encode_to_vec(&fds);

    prost_build::Config::new()
        .out_dir(&out)
        .compile_fds(fds.clone())
        .expect("generating the Rust protobuf types");
    pbjson_build::Builder::new()
        .register_descriptors(&encoded)
        .expect("registering the protobuf descriptors")
        .out_dir(&out)
        // The JSON is what travels, so it reads the way the .proto does:
        // snake_case names, and every field there whether or not it's set.
        .preserve_proto_field_names()
        .emit_fields()
        // An older client talking to a newer server, or the other way round.
        .ignore_unknown_fields()
        .build(&[".ciabatta.oven"])
        .expect("generating the protobuf JSON mapping");

    for file in &fds.file {
        let ts = typescript(file);
        let name = file.name().trim_end_matches(".proto");
        fs::write(out.join(format!("{name}.ts")), ts).expect("writing the TypeScript");
    }
}

/// TypeScript for the JSON a `.proto` file's messages travel as.
///
/// Small on purpose: plain interfaces and string unions, no runtime, because
/// the web app only ever reads and writes JSON. It follows protobuf's JSON
/// mapping as `build.rs` configures pbjson above — field names as written,
/// enums by value name, 64-bit integers as strings.
fn typescript(file: &prost_types::FileDescriptorProto) -> String {
    use prost_types::field_descriptor_proto::{Label, Type};
    use std::fmt::Write;

    // Leading comments, by descriptor path: [4, m] is message m, [4, m, 2, f]
    // its field f, [5, e] enum e, [5, e, 2, v] its value v.
    let mut comments = std::collections::HashMap::new();
    if let Some(info) = &file.source_code_info {
        for location in &info.location {
            if let Some(text) = &location.leading_comments {
                comments.insert(location.path.clone(), text.trim().to_string());
            }
        }
    }
    let doc = |out: &mut String, path: &[i32], indent: &str| {
        if let Some(text) = comments.get(path) {
            let _ = writeln!(out, "{indent}/**");
            for line in text.lines() {
                let _ = writeln!(out, "{indent} * {}", line.trim()).map(|_| ());
            }
            let _ = writeln!(out, "{indent} */");
        }
    };
    let package = format!(".{}.", file.package());
    let short = |name: &str| name.trim_start_matches(&package).replace('.', "_");

    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Generated from proto/{} by build.rs. Don't edit it: change the .proto, then\n\
         // run `CIABATTA_REGENERATE=1 cargo test generated_typescript` to write it here.\n",
        file.name()
    );

    for (e, en) in file.enum_type.iter().enumerate() {
        doc(&mut out, &[5, e as i32], "");
        let values: Vec<String> = en
            .value
            .iter()
            .map(|v| format!("\"{}\"", v.name()))
            .collect();
        let _ = writeln!(
            out,
            "export type {} =\n  | {};\n",
            en.name(),
            values.join("\n  | ")
        );
    }

    for (m, message) in file.message_type.iter().enumerate() {
        doc(&mut out, &[4, m as i32], "");
        let _ = writeln!(out, "export interface {} {{", message.name());
        for (f, field) in message.field.iter().enumerate() {
            doc(&mut out, &[4, m as i32, 2, f as i32], "  ");
            let base = match field.r#type() {
                Type::String | Type::Bytes => "string".to_string(),
                Type::Bool => "boolean".to_string(),
                Type::Int64 | Type::Uint64 | Type::Sint64 | Type::Fixed64 | Type::Sfixed64 => {
                    "string".to_string()
                }
                Type::Message | Type::Enum => short(field.type_name()),
                _ => "number".to_string(),
            };
            let repeated = field.label() == Label::Repeated;
            // `optional`, and any singular message field, may be absent.
            let optional =
                field.proto3_optional() || (field.r#type() == Type::Message && !repeated);
            let ty = if repeated { format!("{base}[]") } else { base };
            let _ = match optional {
                true => writeln!(out, "  {}?: {ty} | null;", field.name()),
                false => writeln!(out, "  {}: {ty};", field.name()),
            };
        }
        let _ = writeln!(out, "}}\n");
    }
    out.trim_end().to_string() + "\n"
}

/// Make sure `<dir>/dist` holds a web app, writing a placeholder if it doesn't.
fn web_dist(root: &Path, dir: &str, workspace: &str, server: &str) {
    let dist = root.join(dir).join("dist");

    // `rerun-if-changed` on a directory only tracks that directory's own mtime,
    // which does not change when a nested file is edited in place. Emit every
    // file individually so an edited asset can't be silently baked in stale.
    println!("cargo:rerun-if-changed={dir}/dist");
    track_recursively(&dist);

    if dist.join("index.html").exists() {
        return;
    }

    if let Err(e) = fs::create_dir_all(&dist) {
        panic!("failed to create {}: {e}", dist.display());
    }
    if let Err(e) = fs::write(dist.join("index.html"), placeholder_html(dir, workspace)) {
        panic!("failed to write the placeholder page: {e}");
    }

    println!(
        "cargo:warning={dir}/dist was empty, so {server} will serve a \
         placeholder page. Run `yarn install && yarn workspace \
         {workspace} build` and rebuild to embed the real web app."
    );
}

/// Make sure `editors/dist` exists, so `include_dir!` has a directory to read.
///
/// No placeholder file: unlike the web app, where a page saying "not built" is
/// more useful than a 404, half an extension is of no use to anyone. An empty
/// directory compiles to an empty download list, which is exactly true.
fn editors_dist(dir: &Path) {
    println!("cargo:rerun-if-changed=editors/dist");
    track_recursively(dir);

    if let Err(e) = fs::create_dir_all(dir) {
        panic!("failed to create {}: {e}", dir.display());
    }
}

/// Emit a `rerun-if-changed` line for every file under `dir`.
fn track_recursively(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            track_recursively(&path);
        }
    }
}

fn placeholder_html(dir: &str, workspace: &str) -> String {
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ciabatta — web app not built</title>
<!-- {PLACEHOLDER_MARKER} -->
<style>
  body {{
    font: 16px/1.6 ui-sans-serif, system-ui, sans-serif;
    max-width: 40rem; margin: 4rem auto; padding: 0 1.5rem;
    color: #1c1917; background: #fafaf9;
  }}
  code {{ background: #e7e5e4; padding: .15em .4em; border-radius: 4px; }}
  pre {{ background: #e7e5e4; padding: 1rem; border-radius: 8px; overflow-x: auto; }}
  @media (prefers-color-scheme: dark) {{
    body {{ color: #e7e5e4; background: #1c1917; }}
    code, pre {{ background: #292524; }}
  }}
</style>
<h1>🍞 The web app isn't built yet</h1>
<p>
  This <code>ciabatta</code> binary was compiled without a
  <code>{dir}/dist</code> bundle, so there's nothing to serve here.
  The server itself is running fine — only the UI is missing.
</p>
<p>Build it and recompile:</p>
<pre>yarn install
yarn workspace {workspace} build
cargo build --release</pre>
<p>
  The JSON API is unaffected; <code>GET /api/health</code> works regardless.
</p>
"#
    )
}
