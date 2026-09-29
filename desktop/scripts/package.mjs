import { spawnSync } from "node:child_process";
import { version } from "./version.mjs";

const resolved = version(true);
const arch = process.argv.includes("--arch");
const deb = process.argv.includes("--deb");
if ((arch || deb) && (process.platform !== "linux" || process.arch !== "x64")) {
  throw new Error("Distribution packages require Linux x86-64");
}
const metadata = [`--config.extraMetadata.version=${resolved}`];
if (arch || deb) {
  const homepage = process.env.CORDIAL_HOMEPAGE;
  if (!homepage || !URL.canParse(homepage) || !/^https?:/.test(homepage)) {
    throw new Error("Set CORDIAL_HOMEPAGE to the project's public homepage for distribution packaging");
  }
  metadata.push(`--config.extraMetadata.homepage=${homepage}`);
}
for (const args of [
  ["scripts/build.mjs", "--production"],
  ["node_modules/electron-builder/cli.js", "--linux", "AppImage", "tar.gz", "dir",
    ...(deb ? ["deb"] : []), ...(arch ? ["pacman"] : []),
    ...(arch || deb ? ["--x64"] : []), "--publish", "never", ...metadata],
]) {
  const result = spawnSync(process.execPath, args, { stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
