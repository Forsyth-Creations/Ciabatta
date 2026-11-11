import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The port the running daemon is on. It picks 80 when it can and 8099 when it
 * can't, and writes down which in ~/.ciabatta/daemon.json — so that is read
 * before falling back to 8099. CIABATTA_DAEMON_PORT still wins.
 */
function daemonPort(): string {
  if (process.env.CIABATTA_DAEMON_PORT) return process.env.CIABATTA_DAEMON_PORT;
  try {
    const record = JSON.parse(readFileSync(join(homedir(), ".ciabatta", "daemon.json"), "utf8"));
    if (record.port) return String(record.port);
  } catch {
    // No daemon recorded yet: the fallback port.
  }
  return "8099";
}
const daemon = `http://127.0.0.1:${daemonPort()}`;

/**
 * The daemon serves this bundle from the root of its own origin, so `base` is
 * "/" — unlike the sibling `frontend` workspace, which is the GitHub Pages docs
 * site and lives under /Ciabatta/.
 *
 * In dev, `yarn dev` runs Vite on 5173 and proxies /api through to a real
 * daemon (port 80, or 8099 — see daemonPort), so HMR works against live data. The proxy also keeps
 * everything same-origin, which is why the daemon needs no CORS layer.
 */
export default defineConfig({
  plugins: [react()],
  base: "/",
  resolve: {
    // `@ciabatta/graphs` is linked source shared with the other app; one copy
    // of each of these, or react-flow and MUI each see two Reacts/themes.
    dedupe: ["react", "react-dom", "@mui/material", "@emotion/react", "@xyflow/react"],
  },
  build: {
    outDir: "dist",
    // The bundle is embedded in the Rust binary, so keep an eye on its size.
    chunkSizeWarningLimit: 900,
  },
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: daemon,
        changeOrigin: true,
      },
      // The config schemas the editor docs link to and offer as downloads.
      // Unauthenticated, and served by the daemon rather than by Vite.
      "/schemas": {
        target: daemon,
        changeOrigin: true,
      },
    },
  },
});
