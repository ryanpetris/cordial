import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import test from "node:test";
import { versionStorageSchema } from "./breaking.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const proto = join(root, "../../../proto");

test("pins the frozen v1 schema independently of the protocol base", () => {
  const archive = readFileSync(join(proto, "storage/v1/storage.proto"));
  assert.equal(createHash("sha256").update(archive).digest("hex"),
    "1960b6d4992e173b15a800c89d6e42beb3f018fc52bc9591325a42f0b78fb237");
});

test("discovers and checks new generated schemas recursively", () => {
  const temporary = mkdtempSync(join(tmpdir(), "cordial-protocol-generate-"));
  try {
    cpSync(proto, join(temporary, "proto"), { recursive: true });
    const packageRoot = join(temporary, "desktop/packages/protocol");
    mkdirSync(packageRoot, { recursive: true });
    symlinkSync(join(root, "../../node_modules"), join(temporary, "desktop/node_modules"), "dir");
    cpSync(join(root, "scripts"), join(packageRoot, "scripts"), { recursive: true });
    for (const path of ["buf.yaml", "buf.gen.yaml"]) {
      cpSync(join(root, path), join(packageRoot, path));
    }
    const schema = join(temporary, "proto/storage/v3/extra.proto");
    mkdirSync(join(temporary, "proto/storage/v3"), { recursive: true });
    writeFileSync(schema, 'syntax = "proto3";\npackage cordial.storage.v3;\nmessage Extra { uint32 format = 1; }\n');
    const run = (...args) => {
      const result = spawnSync(process.execPath, [join(packageRoot, "scripts/generate.mjs"), ...args], {
        cwd: packageRoot, encoding: "utf8",
      });
      assert.equal(result.error, undefined);
      return result;
    };
    let result = run("--check");
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /storage\/v3\/extra_pb\.ts is out of date/);
    result = run();
    assert.equal(result.status, 0, result.stdout + result.stderr);
    const generated = join(packageRoot, "src/gen/storage/v3/extra_pb.ts");
    assert.ok(existsSync(generated));
    result = run("--check");
    assert.equal(result.status, 0, result.stdout + result.stderr);
    writeFileSync(generated, readFileSync(generated, "utf8") + "\n// stale\n");
    result = run("--check");
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /storage\/v3\/extra_pb\.ts is out of date/);
    rmSync(schema);
    result = run("--check");
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /storage\/v3\/extra_pb\.ts is out of date/);
    result = run();
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(existsSync(generated), false);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
});

for (const version of ["v1", "v2"]) {
  test(`normalizes the historical ${version} namespace without changing any fields`, () => {
    const text = readFileSync(join(proto, `storage/${version}/storage.proto`), "utf8");
    const old = text.replace(`package cordial.storage.${version};`, "package cordial.storage;");
    assert.deepEqual(versionStorageSchema(old), { path: `storage/${version}/storage.proto`, text });
  });
}

test("rejects unknown unversioned storage layouts", () => {
  assert.throws(() => versionStorageSchema("message Other {}"));
  assert.throws(() => versionStorageSchema("package cordial.storage;\nmessage Identity {\n  bytes address = 7;\n}"));
});

for (const [schema, before, after] of [
  ["serial/v1/cordial.proto", "string id = 1;", "string hardware_id = 1;"],
  ["migration.proto", "string adapter_id = 1;", "string hardware_id = 1;"],
  ["storage/v1/storage.proto", "uint64 next_device = 1;", "uint64 next_device = 9;"],
  ["storage/v2/storage.proto", "uint64 next_device = 2;", "uint64 next_device = 9;"],
]) {
  test(`WIRE_JSON still rejects incompatible edits to ${schema}`, () => {
    const temporary = mkdtempSync(join(tmpdir(), "cordial-protocol-wire-"));
    try {
      cpSync(proto, temporary, { recursive: true });
      const path = join(temporary, schema);
      const text = readFileSync(path, "utf8");
      assert.ok(text.includes(before));
      writeFileSync(path, text.replace(before, after));
      const result = spawnSync("buf", ["breaking", temporary, "--config", join(root, "buf.yaml"), "--against", proto], { encoding: "utf8" });
      assert.equal(result.error, undefined);
      assert.equal(result.status, 100, result.stdout + result.stderr);
      assert.match(result.stdout + result.stderr, /changed|deleted|same number/i);
    } finally {
      rmSync(temporary, { recursive: true, force: true });
    }
  });
}
