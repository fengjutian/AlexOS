import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * Vite config for the desktop API demo.
 *
 * Notes:
 *  - `base: "./"` keeps asset URLs relative so the built `index.html`
 *    loads correctly when served from a custom protocol by the Alex
 *    WebView (no `/assets/...` absolute path).
 *  - `server.host/port` mirrors the values in `manifest.json`
 *    (`dev.url`). The host enforces `strictPort` so a collision fails
 *    loudly instead of silently drifting to the next free port.
 *  - The `@` alias mirrors the TS path mapping in `tsconfig.json` so
 *    component imports survive a rename without search-and-replace.
 *    The string path is resolved relative to the config file by Vite.
 */
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    sourcemap: true,
  },
  resolve: {
    alias: {
      "@": "./src",
    },
  },
  server: {
    host: "127.0.0.1",
    port: 5174,
    strictPort: true,
  },
  preview: {
    host: "127.0.0.1",
    port: 5174,
    strictPort: true,
  },
});
