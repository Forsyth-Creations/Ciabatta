import { defineConfig } from "vite";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL(".", import.meta.url));

const pkg = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8"),
);

// On release builds the workflow runs on a `v*` tag, so GITHUB_REF_NAME is the
// version (e.g. "v0.1.1"). Use it when present; otherwise fall back to the
// version in package.json. This keeps the displayed version and the release
// download links in lockstep with the actual release without manual edits.
const ref = process.env.GITHUB_REF_NAME || "";
const version = /^v\d/.test(ref) ? ref.replace(/^v/, "") : pkg.version;

export default defineConfig({
  base: "/Ciabatta/",
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },
  build: {
    outDir: "dist",
    rollupOptions: {
      // Multi-page: the landing page plus a dedicated doc page per deploy feature.
      input: {
        main: resolve(root, "index.html"),
        "deploy-env-files": resolve(root, "deploy-env-files.html"),
        "deploy-env-select": resolve(root, "deploy-env-select.html"),
        "deploy-conditional-steps": resolve(root, "deploy-conditional-steps.html"),
      },
    },
  },
});
