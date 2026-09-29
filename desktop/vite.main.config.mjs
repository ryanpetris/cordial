import { builtinModules } from "node:module";
import { defineConfig } from "vite";
import { version } from "./scripts/version.mjs";

export default defineConfig(({ mode }) => ({
  define: { __CORDIAL_VERSION__: JSON.stringify(version()) },
  build: {
    ssr: "src/main/index.ts",
    outDir: "out/main",
    target: "node22",
    sourcemap: mode === "development",
    minify: false,
    rollupOptions: {
      external: ["electron", "serialport", "usb", "usb/index.js", /^node:/, ...builtinModules],
      output: { format: "es", entryFileNames: "index.js" },
    },
  },
  ssr: { noExternal: ["ajv"] },
}));
