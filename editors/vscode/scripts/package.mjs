/**
 * Package the extension into `editors/dist/`, reproducibly.
 *
 * Not `vsce package` on its own, for two reasons.
 *
 * **Where it goes.** The `.vsix` has a second consumer: the daemon embeds
 * everything in `editors/dist/` and serves it at `/extensions/<file>` (see
 * `src/daemon/extensions.rs`), which is what makes the download button on the
 * Editors page of the local web app hand you a file built from the same commit
 * as the binary serving it. `vsce` won't create a missing output directory, and
 * on a fresh clone that directory is missing — it's build output, so it isn't
 * committed. The filename is stable across versions on purpose: it's quoted in
 * the docs, in the release notes, and in the URL the web app links to.
 *
 * **When it changes.** A `.vsix` is a zip, and a zip records the wall-clock
 * time of every entry, so packaging the same sources twice produces two
 * different files. That is enough to defeat ciabatta's own build cache, which
 * compares output hashes: the extension would repackage on every run, and the
 * binary that embeds it would relink on every run behind it. So the timestamps
 * are normalised afterwards and the same input bytes give the same output
 * bytes.
 */
import { spawn } from "node:child_process";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "./build.mjs";

// Zip structure constants, used by the normalisation pass at the bottom of the
// file. Declared up here because `const` is not hoisted and the top-level code
// below runs before that section is reached.

/** 1980-01-01 00:00, the earliest a DOS timestamp can express. */
const DOS_DATE = 0x0021;
const DOS_TIME = 0x0000;

const SIG_LOCAL = 0x04034b50;
const SIG_CENTRAL = 0x02014b50;
const SIG_EOCD = 0x06054b50;

const here = dirname(fileURLToPath(import.meta.url));

/** Where the daemon looks. Shared with `build.rs` and the CI workflow. */
export const outDir = join(here, "..", "..", "dist");
export const outFile = join(outDir, "ciabatta-vscode.vsix");

await mkdir(outDir, { recursive: true });

// Bundle first if nobody has. Running `package` on a fresh checkout is a
// reasonable thing to type, and vsce's failure there ("dist/extension.js does
// not exist") sends you looking for a packaging bug rather than a missing
// step. When the graph in `.ciabatta/workflows/build.yaml` ran the bundle
// already, this finds it and skips.
const bundled = join(here, "..", "dist", "extension.js");
if (!(await access(bundled).then(() => true, () => false))) {
  console.log("no bundle yet — building one first");
  await build();
}

// `--no-dependencies` because esbuild already bundled everything the extension
// imports into dist/extension.js; vsce would otherwise walk node_modules and
// pack a second, unused copy of vscode-languageclient.
await new Promise((resolve, reject) => {
  const vsce = spawn("yarn", ["vsce", "package", "--no-dependencies", "--out", outFile], {
    cwd: join(here, ".."),
    stdio: "inherit",
    shell: process.platform === "win32",
  });
  vsce.on("error", reject);
  vsce.on("exit", (code) =>
    code === 0 ? resolve() : reject(new Error(`vsce exited with ${code}`)),
  );
});

await normaliseTimestamps(outFile);
console.log(`packaged: ${outFile}`);

// ─── Reproducible zips ──────────────────────────────────────────────────────

/**
 * Rewrite every entry's modified time to a fixed value, in place.
 *
 * The structure is walked rather than scanned for signatures: compressed file
 * data can contain any four bytes, including something that looks like a local
 * file header, and patching one of those would corrupt the archive. Starting
 * from the end-of-central-directory record and following it to each entry means
 * only real headers are ever touched.
 *
 * Only two bytes of time and two of date are overwritten per header, so nothing
 * moves and no offset needs fixing. Zip stores a CRC of the file *data*, not of
 * the headers, so the checksums stay valid.
 */
async function normaliseTimestamps(path) {
  const zip = await readFile(path);

  const eocd = findEocd(zip);
  const entries = zip.readUInt16LE(eocd + 10);
  let cursor = zip.readUInt32LE(eocd + 16);

  for (let i = 0; i < entries; i += 1) {
    if (zip.readUInt32LE(cursor) !== SIG_CENTRAL) {
      throw new Error(`${path}: central directory entry ${i} is not where the archive says`);
    }

    // Central directory entry: time at +12, date at +14.
    zip.writeUInt16LE(DOS_TIME, cursor + 12);
    zip.writeUInt16LE(DOS_DATE, cursor + 14);

    const nameLength = zip.readUInt16LE(cursor + 28);
    const extraLength = zip.readUInt16LE(cursor + 30);
    const commentLength = zip.readUInt16LE(cursor + 32);
    const localOffset = zip.readUInt32LE(cursor + 42);

    if (zip.readUInt32LE(localOffset) !== SIG_LOCAL) {
      throw new Error(`${path}: entry ${i} does not point at a local file header`);
    }
    // Local file header: time at +10, date at +12.
    zip.writeUInt16LE(DOS_TIME, localOffset + 10);
    zip.writeUInt16LE(DOS_DATE, localOffset + 12);

    cursor += 46 + nameLength + extraLength + commentLength;
  }

  await writeFile(path, zip);
}

/**
 * The offset of the end-of-central-directory record.
 *
 * It is last in the file but variable-length, because it ends with a comment,
 * so it has to be searched for backwards. The comment can be 64 KB, which
 * bounds how far back to look.
 */
function findEocd(zip) {
  const earliest = Math.max(0, zip.length - 22 - 0xffff);
  for (let i = zip.length - 22; i >= earliest; i -= 1) {
    if (zip.readUInt32LE(i) === SIG_EOCD) return i;
  }
  throw new Error("not a zip archive: no end-of-central-directory record");
}
