# Ciabatta for VS Code

Completion, documentation and reference checking for `.ciabatta/` files.

## What it does

Inside `.ciabatta/ciabatta.yaml` and `.ciabatta/workflows/*.yaml`:

- **Every field, with its documentation.** What `persistent:` means, why
  `continue_on_error:` exists, what a `timeout:` accepts. From the JSON Schema,
  so it's the same text the format's own reference uses.
- **Dependencies that resolve.** A step's `needs:` offers the steps in that
  file. A workflow's `needs:` offers the *other packages'* workflows —
  `proto:generate`, with its description beside it. Two fields spelled the same
  way that mean different things, which is the mistake this exists to prevent.
- **Tools your repo can actually install.** `requires:` offers what the
  monorepo root's `toolchain:` has a hint for. Anything else gets a warning:
  a missing tool with no install command is a build failure with no fix.
- **Registries, tags, `{CIABATTA_*}` variables**, and the phases `kind:` knows.
- **Typo detection.** `needs: [protos]` is flagged where you typed it, with
  "Did you mean `proto`?", rather than at build time in someone else's
  terminal.

## Installing

The extension depends on [YAML by Red
Hat](https://marketplace.visualstudio.com/items?itemName=redhat.vscode-yaml),
which VS Code installs alongside it. That half needs nothing else.

For the repository-aware half, put the `ciabatta` CLI on your `PATH`:

```sh
cargo install ciabatta
```

The extension runs `ciabatta lsp` and talks to it over stdio. Without the
binary, field completion still works and nothing complains.

## Settings

| Setting | Default | |
| --- | --- | --- |
| `ciabatta.server.path` | `""` | A specific binary, for a build of your own. Empty uses `PATH`. |
| `ciabatta.server.enabled` | `true` | Turn the server off and keep only the schema. |
| `ciabatta.trace.server` | `off` | Log the protocol traffic to the **Ciabatta** output channel. |

**Ciabatta: Restart Language Server** picks up a rebuilt binary without
reloading the window.

## Developing

From the repository root:

```sh
yarn install
yarn workspace ciabatta-vscode build     # bundle to dist/, copying in the schemas
yarn workspace ciabatta-vscode dev       # the same, watching
```

Then <kbd>F5</kbd> in VS Code with `editors/vscode` open to launch an
Extension Development Host.

The schemas are copied in from `editors/schemas/` at build time and are not
checked in here — Zed uses the same files, and a schema kept in two places
describes two different formats within a year. Edit the originals.
