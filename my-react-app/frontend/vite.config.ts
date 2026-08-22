import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Vite emits a single index.html at the package root (./index.html
// relative to this config file) so the Alex OS host can serve it
// without rewriting paths. The bundle lives next to it.
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: ".",
    emptyOutDir: true,
    sourcemap: true,
  },
});
