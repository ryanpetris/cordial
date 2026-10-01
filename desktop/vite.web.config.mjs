import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig(({ mode }) => ({
  root: `${import.meta.dirname}/src/web`,
  base: "./",
  plugins: [react()],
  build: {
    outDir: `${import.meta.dirname}/out/web`,
    emptyOutDir: true,
    target: "chrome140",
    sourcemap: mode === "development",
  },
}));
