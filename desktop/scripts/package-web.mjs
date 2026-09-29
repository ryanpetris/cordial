// Release package of the web version: a directory to upload as is to a
// static host such as GitHub Pages or Cloudflare Pages.
import { spawnSync } from "node:child_process";
import { cpSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { version } from "./version.mjs";

const resolved = version(true);
const result = spawnSync(process.execPath, ["scripts/build-web.mjs", "--production"], { stdio: "inherit" });
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
const packages = fileURLToPath(new URL("../../build/packages/web/", import.meta.url));
const destination = `${packages}cordial-web-${resolved}`;
rmSync(packages, { recursive: true, force: true });
cpSync(fileURLToPath(new URL("../out/web/", import.meta.url)), destination, { recursive: true });
cpSync(fileURLToPath(new URL("../../LICENSE", import.meta.url)), `${destination}/LICENSE`);
console.log(`web package in ${destination}`);
