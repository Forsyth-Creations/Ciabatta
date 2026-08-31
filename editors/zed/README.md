# Ciabatta for Zed

Completion and reference checking for `.ciabatta/` files, from the same
language server the VS Code extension uses.

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
rustup target add wasm32-wasip1
cargo build --manifest-path editors/zed/Cargo.toml --target wasm32-wasip1
```

Zed rebuilds a dev extension itself when you reload it, so this is only for
seeing compile errors without leaving the terminal.
