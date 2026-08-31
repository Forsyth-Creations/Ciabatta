/**
 * Bundle the extension.
 *
 * A script rather than a chain of shell in `package.json`, for two reasons.
 * The schemas have to be copied in before esbuild runs, and expressing that as
 * `yarn run schemas && esbuild …` meant `yarn dev` could only add flags by
 * appending them to the end of that string — which silently landed them after
 * `--minify`, so the watch build you debugged against was minified. And the
 * two builds genuinely differ: one is for reading in a debugger, the other is
 * what ships. Saying so in JavaScript is shorter than encoding it in npm-script
 * argument forwarding.
 */
import { fileURLToPath } from "node:url";

import * as esbuild from "esbuild";

import { copySchemas, target } from "./schemas.mjs";

const watch = process.argv.includes("--watch");

/**
 * `vscode` is provided by the editor at runtime and must never be bundled;
 * everything else is, so the published extension is one file with no
 * `node_modules` beside it.
 */
const options = {
  entryPoints: ["src/extension.ts"],
  outfile: "dist/extension.js",
  bundle: true,
  external: ["vscode"],
  format: "cjs",
  platform: "node",
  // The oldest Node any supported VS Code embeds.
  target: "node18",
  logLevel: "info",
  // Readable in a debugger while developing; small on the marketplace.
  minify: !watch,
  sourcemap: watch,
};

/**
 * Copy the schemas in and bundle. Exported so `package.mjs` can produce a
 * bundle when there isn't one, rather than leaving vsce to fail on the
 * missing file.
 */
export async function build() {
  await copySchemas();
  console.log(`schemas: copied into ${target}`);

  if (watch) {
    const context = await esbuild.context(options);
    await context.watch();
    console.log("watching for changes — ^C to stop");
  } else {
    await esbuild.build(options);
  }
}

// Run when invoked directly: `node scripts/build.mjs`.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await build();
}
