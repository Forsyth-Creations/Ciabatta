import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The daemon serves this bundle from the root of its own origin, so `base` is
 * "/" — unlike the sibling `frontend` workspace, which is the GitHub Pages docs
 * site and lives under /Ciabatta/.
 *
 * In dev, `yarn dev` runs Vite on 5173 and proxies /api through to a real
 * daemon on 8099, so HMR works against live data. The proxy also keeps
 * everything same-origin, which is why the daemon needs no CORS layer.
 */
export default defineConfig({
  plugins: [react()],
  base: "/",
  build: {
    outDir: "dist",
    // The bundle is embedded in the Rust binary, so keep an eye on its size.
    chunkSizeWarningLimit: 900,
  },
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: `http://127.0.0.1:${process.env.CIABATTA_DAEMON_PORT ?? 8099}`,
        changeOrigin: true,
      },
    },
  },
});
