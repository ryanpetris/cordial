// Compares serial/IPC schemas and each storage format with the same format in an earlier ref.
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/** Maps an unversioned historical schema to its existing storage format without changing fields. */
export function versionStorageSchema(text) {
  const identity = text.match(/message Identity\s*\{([\s\S]*?)\n\}/)?.[1];
  if (!identity) throw new Error("Historical storage schema has no Identity record");
  let version;
  if (/\buint32 format = 1;/.test(identity) && /\bbytes address = 2;/.test(identity)) version = "v2";
  else if (!/\bformat\s*=/.test(identity) && /\bbytes address = 1;/.test(identity)) version = "v1";
  else throw new Error("Historical storage schema has an unknown format");
  if (!text.includes("package cordial.storage;")) throw new Error("Unexpected historical storage package");
  return {
    path: `storage/${version}/storage.proto`,
    text: text.replace("package cordial.storage;", `package cordial.storage.${version};`),
  };
}

function breaking(ref) {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const repository = join(root, "../../..");
  const temporary = mkdtempSync(join(tmpdir(), "cordial-protocol-base-"));
  try {
    const files = execFileSync("git", ["ls-tree", "-r", "--name-only", ref, "--", "proto/"], {
      cwd: repository, encoding: "utf8",
    }).trim().split("\n").filter((path) => path.endsWith(".proto"));
    if (!files.includes("proto/cordial.proto")) throw new Error("Protocol base has no serial schema");
    for (const file of files) {
      let path = file.slice("proto/".length);
      let text = execFileSync("git", ["show", `${ref}:${file}`], { cwd: repository, encoding: "utf8" });
      if (path === "storage.proto") ({ path, text } = versionStorageSchema(text));
      const destination = join(temporary, path);
      mkdirSync(dirname(destination), { recursive: true });
      writeFileSync(destination, text);
    }
    // WIRE_JSON still covers every serial/IPC field and every retained storage namespace.
    execFileSync("buf", ["breaking", join(repository, "proto"), "--config", join(root, "buf.yaml"), "--against", temporary], {
      cwd: root, stdio: "inherit",
    });
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const ref = process.argv[2];
  if (!ref) throw new Error("Usage: node scripts/breaking.mjs <git-ref>");
  breaking(ref);
}
