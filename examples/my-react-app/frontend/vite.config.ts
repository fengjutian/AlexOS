import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Source lives at frontend/src/ with the Vite entry in
// frontend/index.html. `outDir: "dist"` keeps the build output
// in frontend/dist/ so it doesn't clobber the source files
// (Vite would warn and `emptyOutDir: true` would wipe them).
// The Alex OS manifest points its `frontend.entry` at
// frontend/dist/index.html so the host serves the built bundle.
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
  },
});
