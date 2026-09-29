import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import { precompiledValidators } from "./scripts/web-validators.mjs";

export default defineConfig(({ mode }) => ({
  root: `${import.meta.dirname}/src/web`,
  base: "./",
  plugins: [react(), precompiledValidators()],
  build: {
    outDir: `${import.meta.dirname}/out/web`,
    emptyOutDir: true,
    target: "chrome140",
    sourcemap: mode === "development",
    // The precompiled schema validators are one large chunk.
    chunkSizeWarningLimit: 2048,
  },
}));
