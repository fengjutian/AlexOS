import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Vite emits a single index.html at the package root (./index.html
// relative to this config file) so the Alex OS host can serve it
// without rewriting paths. The bundle lives next to it.
// `base: "./"` makes the build emit relative URLs in index.html
// (e.g. `./assets/index-XXX.js`) so the alex://app/ asset router
// can resolve them via the existing path-join mapping.
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: ".",
    emptyOutDir: true,
    sourcemap: true,
  },
});
