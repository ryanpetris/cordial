import { version } from "./version.mjs";
// Builds the main process, preload script and window with Vite.
import react from "@vitejs/plugin-react";
import { builtinModules } from "node:module";
import { rmSync } from "node:fs";
import { build } from "vite";

const root = new URL("../", import.meta.url).pathname;
const production = process.argv.includes("--production");
const external = ["electron", "serialport", "usb", "usb/index.js", /^node:/, ...builtinModules];
rmSync(`${root}out`, { recursive: true, force: true });

const resolvedVersion = version();
const common = { define: { __CORDIAL_VERSION__: JSON.stringify(resolvedVersion) }, configFile: false, logLevel: "warn", root, mode: production ? "production" : "development" };

await build({
  ...common,
  build: {
    ssr: "src/main/index.ts",
    outDir: "out/main",
    target: "node22",
    sourcemap: !production,
    minify: false,
    rollupOptions: { external, output: { format: "es", entryFileNames: "index.js" } },
  },
  ssr: { noExternal: ["ajv"] },
});

await build({
  ...common,
  build: {
    ssr: "src/preload/index.ts",
    outDir: "out/preload",
    target: "node22",
    minify: false,
    rollupOptions: { external, output: { format: "cjs", entryFileNames: "index.cjs" } },
  },
});

await build({
  ...common,
  root: `${root}src/renderer`,
  base: "./",
  plugins: [react()],
  build: { outDir: `${root}out/renderer`, emptyOutDir: true, target: "chrome140", sourcemap: !production },
});
