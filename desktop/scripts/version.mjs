import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export function version(release = false) {
  const candidates = process.env.CORDIAL_PYTHON ? [process.env.CORDIAL_PYTHON] : ["python3", "python", "py"];
  for (const executable of candidates) {
    const prefix = executable === "py" ? ["-3"] : [];
    const probe = spawnSync(executable, [...prefix, "-c", "import sys; sys.exit(sys.version_info < (3, 11))"]);
    if (probe.error || probe.status !== 0) continue;
    const result = spawnSync(executable, [...prefix,
      fileURLToPath(new URL("../../tools/version.py", import.meta.url)),
      ...(release ? ["--release"] : [])], { encoding: "utf8" });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(result.stderr.trim());
    return result.stdout.trim();
  }
  throw new Error("Cordial builds require Python 3.11+; install it or set CORDIAL_PYTHON to its executable");
}
