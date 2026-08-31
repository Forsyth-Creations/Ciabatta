/**
 * Copy the shared JSON Schemas into the extension.
 *
 * They live in `editors/schemas/` because Zed and the daemon need the same
 * files, and a schema kept in three places describes three different formats
 * by the end of the year. `vsce` can only package what's under the extension
 * root, so the build brings a copy in rather than reaching outside.
 */
import { cp, mkdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
export const source = join(here, "..", "..", "schemas");
export const target = join(here, "..", "schemas");

/** Replace the extension's copy with the current originals. Idempotent. */
export async function copySchemas() {
  await rm(target, { recursive: true, force: true });
  await mkdir(target, { recursive: true });
  await cp(source, target, { recursive: true });
}

// Also usable on its own: `node scripts/schemas.mjs`.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await copySchemas();
  console.log(`schemas: ${source} -> ${target}`);
}
