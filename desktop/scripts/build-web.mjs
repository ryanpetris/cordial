// Builds the web version into out/web: the window and the controller in one
// static page that reaches adapters through Web Serial. It is separate from
// the Electron build and its packages.
import react from "@vitejs/plugin-react";
import { build } from "vite";
import { precompiledValidators } from "./web-validators.mjs";

const root = new URL("../", import.meta.url).pathname;
const production = process.argv.includes("--production");

await build({
  configFile: false,
  logLevel: "warn",
  root: `${root}src/web`,
  // Relative URLs, so the page works under any path of a static host.
  base: "./",
  mode: production ? "production" : "development",
  plugins: [react(), precompiledValidators()],
  build: {
    outDir: `${root}out/web`,
    emptyOutDir: true,
    target: "chrome140",
    sourcemap: !production,
    // The precompiled schema validators are one large chunk.
    chunkSizeWarningLimit: 2048,
  },
});
