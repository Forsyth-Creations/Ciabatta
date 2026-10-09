/**
 * Copy the shared JSON Schemas into the extension.
 *
 * They live in `editors/schemas/` because Zed and the daemon need the same
 * files, and a schema kept in three places describes three different formats
 * by the end of the year. `vsce` can only package what's under the extension
 * root, so the build brings a copy in rather than reaching outside.
 *
 * The copy is adjusted for VS Code on the way in (see `forVscode`), so the
 * originals stay plain JSON Schema that any consumer can read.
 */
import { mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
export const source = join(here, "..", "..", "schemas");
export const target = join(here, "..", "schemas");

/**
 * Example values that `ciabatta lsp` replaces with real ones from the repo.
 * Left in, the YAML extension offers them next to the server's suggestions:
 * a `proto:generate` this repo has never heard of, sitting above the
 * workflows it actually has. Keyed by file, as JSON Pointers.
 */
const SERVED_BY_LSP = {
  "common.schema.json": [
    "/$defs/workflowRef",
    "/$defs/step/properties/requires",
    "/$defs/step/properties/kind",
  ],
  "workflow.schema.json": ["/properties/needs", "/properties/background"],
};

/**
 * Adjust one schema for VS Code: drop the examples listed above, and mirror
 * every `description` into a `markdownDescription`. The YAML extension renders
 * `description` as plain text, so the backticks the schemas use for field
 * names show up literally; the markdown field renders them as code.
 */
export function forVscode(file, schema) {
  for (const pointer of SERVED_BY_LSP[file] ?? []) {
    const node = pointer
      .split("/")
      .slice(1)
      .reduce((n, key) => n?.[key], schema);
    if (!node) throw new Error(`${file} has no ${pointer} — update SERVED_BY_LSP`);
    delete node.examples;
  }

  const visit = (node) => {
    if (Array.isArray(node)) return node.forEach(visit);
    if (!node || typeof node !== "object") return;
    if (typeof node.description === "string" && node.markdownDescription === undefined) {
      node.markdownDescription = node.description;
    }
    // Snippet bodies are the YAML to insert, not schema; leave them alone.
    for (const [key, value] of Object.entries(node)) {
      if (key !== "defaultSnippets") visit(value);
    }
  };
  visit(schema);
  return schema;
}

/** Replace the extension's copy with the current originals. Idempotent. */
export async function copySchemas() {
  await rm(target, { recursive: true, force: true });
  await mkdir(target, { recursive: true });
  for (const file of await readdir(source)) {
    if (!file.endsWith(".json")) continue;
    const schema = JSON.parse(await readFile(join(source, file), "utf8"));
    await writeFile(join(target, file), `${JSON.stringify(forVscode(file, schema), null, 2)}\n`);
  }
}

// Also usable on its own: `node scripts/schemas.mjs`.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await copySchemas();
  console.log(`schemas: ${source} -> ${target}`);
}
