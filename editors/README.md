# Editor support

Two halves, and they know different things.

**The JSON Schemas** in `schemas/` describe the *shape* of ciabatta's config
files: which fields exist, what each takes, what it's for, and which values are
legal. They're plain JSON Schema, so any editor with YAML support can use them,
and they need no binary installed. This is where field-name completion, hover
documentation and typo detection come from.

**The language server** is `ciabatta lsp`, a subcommand of the CLI you already
have. It knows the things a schema can't: which sub-workspaces *this* monorepo
contains, which workflows they define, which tools the root's `toolchain:`
promises to install, which registries are configured. That's what fills in a
`needs:` and warns when one points at something that isn't there.

The editor extensions are thin. They register the schemas and launch the
server; neither contains any knowledge of the format, which is why the two
editors can't drift apart.

```
editors/
├── schemas/          The JSON Schemas — shared, editor-agnostic
├── vscode/           VS Code extension (TypeScript)
└── zed/              Zed extension (Rust → WASM)
```

## What each half gives you

| | Schemas | `ciabatta lsp` |
| --- | --- | --- |
| Field names, with docs | ✅ | |
| Enum values (`kind: push`, `format: npm`) | ✅ | |
| Wrong type, unknown field, missing `name:` | ✅ | |
| `needs:` → the other packages' workflows | | ✅ |
| `needs:` inside a step → the steps in this file | | ✅ |
| `requires:` → the tools the root can install | | ✅ |
| `registry:` → the configured registries | | ✅ |
| `tags:` → the ones this repo already uses | | ✅ |
| `{CIABATTA_*}` in a `publish_path` | | ✅ |
| "No sub-workspace defines `protos`. Did you mean `proto`?" | | ✅ |

## The schemas on their own

Nothing about them is editor-specific. To validate a config in CI, or to wire
up an editor neither extension covers, point any JSON Schema tool at:

| File | Schema |
| --- | --- |
| `.ciabatta/ciabatta.yaml` | `schemas/ciabatta.schema.json` |
| `.ciabatta/workflows/*.yaml` | `schemas/workflow.schema.json` |

Both `$ref` into `schemas/common.schema.json`, which holds the step and cache
definitions they share — keep the three files together.
