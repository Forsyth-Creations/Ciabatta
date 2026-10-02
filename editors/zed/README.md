# Ciabatta for Zed

Completion, reference checking and run status for `.ciabatta/` files, from
the same language server the VS Code extension uses.

## Installing

Zed extensions can't ship a binary, and shouldn't: `ciabatta lsp` is the same
executable that runs your builds, and an editor quietly fetching a second copy
at some other version is how completions start disagreeing with
`ciabatta build`. So put the CLI on your `PATH` first:

```sh
cargo install ciabatta
```

Then install the extension. Until it's in Zed's registry, use
**zed: install dev extension** and pick `editors/zed`.

To point it at a build of your own, in `.zed/settings.json`:

```json
{
  "lsp": {
    "ciabatta": {
      "binary": { "path": "./target/release/ciabatta" }
    }
  }
}
```

## Run status

How the project's runs are going shows up where Zed already shows things:

- **In the status bar, while a run is going.** A run started from the web app
  (or anything else that goes through the daemon) shows as progress —
  `ciabatta build · 3/7 steps · api:compile` — until it ends, with how it ended
  and how long it took.
- **As a notification, when one fails**, with a link to the run's page. A
  success doesn't interrupt you; the status bar already said so.
- **On the workflow file, if its last run failed.** A warning on the file's
  first line — in the project diagnostics too — saying when, after how long,
  and how often that workflow fails here. It clears when a run passes.
- **On hover.** Hover a top-level line of a workflow file (`description:`,
  `steps:`) for how its last run went, and whether one is running now.

Live progress needs the daemon running, which anything that opens the web app
starts. The last-run status comes from `.ciabatta/history/`, which every run
writes wherever it was started from, so it works without one.

## The schemas

The extension covers the repository-aware half — the workflows a `needs:` can
name, the tools the root can install, the registries a `push` step can use.

Field names and their documentation come from the JSON Schema, which Zed's
built-in `yaml-language-server` reads. That one wants a settings block, because
Zed has no equivalent of the contribution point VS Code uses. In your project's
`.zed/settings.json`:

```json
{
  "lsp": {
    "yaml-language-server": {
      "settings": {
        "yaml": {
          "schemas": {
            "https://forsyth-creations.github.io/Ciabatta/schemas/ciabatta.schema.json": [
              ".ciabatta/ciabatta.yaml",
              "**/.ciabatta/ciabatta.yaml"
            ],
            "https://forsyth-creations.github.io/Ciabatta/schemas/workflow.schema.json": [
              ".ciabatta/workflows/*.yaml",
              "**/.ciabatta/workflows/*.yaml"
            ]
          }
        }
      }
    }
  }
}
```

Both keys may also be a path relative to the worktree root — point them at
`editors/schemas/…` to try a change to the schemas before publishing it.

## Developing

```sh
rustup target add wasm32-wasip2
cargo build --manifest-path editors/zed/Cargo.toml --target wasm32-wasip2
```

Zed rebuilds a dev extension itself when you reload it, so this is only for
seeing compile errors without leaving the terminal.

## Publishing

Registry submissions go to [`zed-industries/extensions`][extensions] as a
submodule PR. Because the extension lives in a subdirectory of this repo rather
than at its root, the entry needs a `path`:

```toml
[ciabatta-lsp]
submodule = "extensions/ciabatta-lsp"
path = "editors/zed"
version = "0.3.6"
```

`version` has to match `extension.toml` at the submodule commit, and the
`LICENSE` in this directory is the one their CI checks — the repository root's
does not count for an extension in a subdirectory.

[extensions]: https://github.com/zed-industries/extensions
