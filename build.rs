//! Makes sure `tool_frontend/dist` exists before the crate is compiled.
//!
//! The daemon embeds the built web app with `include_dir!`, which needs the
//! directory to be present at compile time. On a fresh clone it isn't — the
//! bundle is produced by `yarn turbo run build --filter=ciabatta-tool-frontend`
//! and is gitignored. Rather than fail the build (which would mean nobody can
//! `cargo build` without node installed), we drop in a placeholder page that
//! tells the reader how to build the real thing.
//!
//! CI and the release workflow run the yarn build *before* cargo, so shipped
//! binaries always carry the real app. See the "Release check" in the README.

use std::fs;
use std::path::Path;

/// Marker text also used by the release check to detect a placeholder build.
const PLACEHOLDER_MARKER: &str = "ciabatta-placeholder-bundle";

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let dist = Path::new(&manifest_dir).join("tool_frontend").join("dist");

    // `rerun-if-changed` on a directory only tracks that directory's own mtime,
    // which does not change when a nested file is edited in place. Emit every
    // file individually so an edited asset can't be silently baked in stale.
    println!("cargo:rerun-if-changed=tool_frontend/dist");
    track_recursively(&dist);

    if dist.join("index.html").exists() {
        return;
    }

    if let Err(e) = fs::create_dir_all(&dist) {
        panic!("failed to create {}: {e}", dist.display());
    }
    if let Err(e) = fs::write(dist.join("index.html"), placeholder_html()) {
        panic!("failed to write the placeholder page: {e}");
    }

    println!(
        "cargo:warning=tool_frontend/dist was empty, so the daemon will serve a \
         placeholder page. Run `yarn install && yarn turbo run build \
         --filter=ciabatta-tool-frontend` and rebuild to embed the real web app."
    );
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

fn placeholder_html() -> String {
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
  <code>tool_frontend/dist</code> bundle, so there's nothing to serve here.
  The daemon itself is running fine — only the UI is missing.
</p>
<p>Build it and recompile:</p>
<pre>yarn install
yarn turbo run build --filter=ciabatta-tool-frontend
cargo build --release</pre>
<p>
  The JSON API is unaffected; <code>GET /api/health</code> works regardless.
</p>
"#
    )
}
