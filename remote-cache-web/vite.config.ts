import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The remote cache server embeds this bundle and serves it from the root of
 * its own origin, so `base` is "/".
 *
 * In dev, `yarn dev` runs Vite on 5174 and proxies /api through to a real
 * cache server (8380 by default — `ciabatta remote-cache start`'s port), so
 * HMR works against live numbers and everything stays same-origin.
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
    port: 5174,
    proxy: {
      "/api": {
        target: process.env.CIABATTA_CACHE_URL ?? "http://127.0.0.1:8380",
        changeOrigin: true,
        secure: false,
      },
    },
  },
});
