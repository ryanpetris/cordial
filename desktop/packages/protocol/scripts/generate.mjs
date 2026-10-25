// Generates the serial, migration IPC and versioned storage messages with buf and
// protoc-gen-es. `--check` fails when the checked-in output differs from
// regeneration.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const proto = join(root, "../../../proto");
const check = process.argv.includes("--check");

function generatedFiles(directory) {
  return existsSync(directory)
    ? readdirSync(directory, { recursive: true }).filter((path) => path.endsWith("_pb.ts")).sort()
    : [];
}

function messages(out) {
  // buf only reads module paths inside the current directory, so the schema
  // directory is the input and this package's buf.yaml is passed explicitly.
  execFileSync("buf", ["generate", proto, "--config", join(root, "buf.yaml"), "--template", join(root, "buf.gen.yaml"), "--output", out], {
    cwd: root,
    stdio: "inherit",
  });
}

const temporary = mkdtempSync(join(tmpdir(), "cordial-protocol-"));
try {
  messages(temporary);
  const output = join(temporary, "src/gen");
  const checkedIn = join(root, "src/gen");
  const paths = new Set([...generatedFiles(output), ...generatedFiles(checkedIn)]);
  for (const path of paths) {
    const generated = join(output, path);
    const file = join(checkedIn, path);
    if (!check) {
      if (existsSync(generated)) {
        mkdirSync(dirname(file), { recursive: true });
        writeFileSync(file, readFileSync(generated));
      } else {
        rmSync(file);
      }
    } else if (!existsSync(generated) || !existsSync(file) || !readFileSync(file).equals(readFileSync(generated))) {
      console.error(`src/gen/${path} is out of date; run npm run generate`);
      process.exitCode = 1;
    }
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
