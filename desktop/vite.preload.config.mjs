import { builtinModules } from "node:module";
import { defineConfig } from "vite";

export default defineConfig({
  build: {
    ssr: "src/preload/index.ts",
    outDir: "out/preload",
    target: "node22",
    minify: false,
    rollupOptions: {
      external: ["electron", /^node:/, ...builtinModules],
      output: { format: "cjs", entryFileNames: "index.cjs" },
    },
  },
});
