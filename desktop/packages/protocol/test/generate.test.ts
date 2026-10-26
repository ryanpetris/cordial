import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";

const source = fileURLToPath(new URL("../../../../", import.meta.url));

it("checks retained digests and generates current serial messages with migration IPC and storage", () => {
  const root = mkdtempSync(join(tmpdir(), "cordial-generation-"));
  const protocol = join(root, "desktop/packages/protocol");
  function write(path: string, content: string) {
    const file = join(root, path);
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, content);
  }
  const generate = (...args: string[]) => execFileSync(process.execPath, [join(protocol, "scripts/generate.mjs"), ...args], {
    cwd: root,
    env: { ...process.env, PATH: `${source}desktop/node_modules/.bin:${process.env.PATH}` },
    stdio: "pipe",
  });
  try {
    mkdirSync(protocol, { recursive: true });
    symlinkSync(`${source}desktop/node_modules`, join(root, "desktop/node_modules"), "dir");
    cpSync(`${source}desktop/packages/protocol/scripts`, join(protocol, "scripts"), { recursive: true });
    for (const path of ["buf.yaml", "buf.gen.yaml"]) cpSync(`${source}desktop/packages/protocol/${path}`, join(protocol, path));
    write("proto/handshake.proto", readFileSync(`${source}proto/handshake.proto`, "utf8"));
    write("proto/serial/current", "2\n");
    for (const path of ["migration.proto", "storage/v1/storage.proto", "storage/v2/storage.proto"]) {
      write(`proto/${path}`, readFileSync(`${source}proto/${path}`, "utf8"));
    }
    write("proto/serial/v1/cordial.proto", 'syntax = "proto3"; package fixture.serial.v1; message Request { uint32 id = 1; }');
    const imported = 'syntax = "proto3"; package fixture.shared; message Value { uint32 value = 1; }';
    write("proto/shared/common.proto", imported);
    write("proto/serial/v2/cordial.proto", 'syntax = "proto3"; package fixture.serial.v2; import "shared/common.proto"; message Request { fixture.shared.Value value = 1; }');
    generate();
    generate("--check");
    expect(readFileSync(join(protocol, "src/gen/serial/v2/cordial_pb.ts"), "utf8")).toContain('"../../shared/common_pb.ts"');
    expect(existsSync(join(protocol, "src/gen/shared/common_pb.ts"))).toBe(true);
    expect(existsSync(join(protocol, "src/gen/serial/v1"))).toBe(false);
    expect(existsSync(join(protocol, "src/gen/storage_pb.ts"))).toBe(false);
    for (const path of ["migration_pb.ts", "storage/v1/storage_pb.ts", "storage/v2/storage_pb.ts"]) {
      expect(existsSync(join(protocol, "src/gen", path))).toBe(true);
    }
    const operational = readFileSync(join(protocol, "src/current.ts"), "utf8");
    write("proto/handshake.proto", readFileSync(`${source}proto/handshake.proto`, "utf8").replace("string name = 4", "string display_name = 4"));
    generate();
    expect(readFileSync(join(protocol, "src/gen/handshake_pb.ts"), "utf8")).toContain("displayName?");
    expect(readFileSync(join(protocol, "src/current.ts"), "utf8")).toBe(operational);
    write("proto/migration.proto", readFileSync(`${source}proto/migration.proto`, "utf8").replace("string adapter_id", "string hardware_id"));
    write("proto/storage/v2/storage.proto", readFileSync(`${source}proto/storage/v2/storage.proto`, "utf8").replace("uint64 next_device", "string next_device"));
    generate();
    expect(readFileSync(join(protocol, "src/current.ts"), "utf8")).toBe(operational);
    expect(readFileSync(join(protocol, "src/gen/cordial_pb.ts"), "utf8")).toContain('"./serial/v2/cordial_pb.ts"');

    execFileSync(`${source}desktop/node_modules/.bin/tsc`, [
      "--noEmit", "--allowImportingTsExtensions", "--target", "ESNext", "--module", "ESNext",
      "--moduleResolution", "Bundler", "--skipLibCheck", join(protocol, "src/gen/cordial_pb.ts"),
      join(protocol, "src/gen/migration_pb.ts"), join(protocol, "src/gen/storage/v1/storage_pb.ts"),
      join(protocol, "src/gen/storage/v2/storage_pb.ts"),
    ], { cwd: root, stdio: "pipe" });

    const historical = readFileSync(join(root, "proto/serial/v1/schema.sha256"), "utf8");
    write("proto/serial/v1/schema.sha256", "invalid\n");
    expect(() => generate("--check")).toThrow();
    write("proto/serial/v1/schema.sha256", historical);
    write("proto/serial/v1/cordial.proto", 'syntax = "proto3"; package fixture.serial.v1; message Request { string id = 1; }');
    expect(() => generate("--check")).toThrow();
    generate();
    expect(readFileSync(join(root, "proto/serial/v1/schema.sha256"), "utf8")).not.toBe(historical);

    const current = readFileSync(join(protocol, "src/current.ts"), "utf8");
    write("proto/shared/common.proto", imported.replace("uint32 value", "string value"));
    expect(() => generate("--check")).toThrow();
    generate();
    expect(readFileSync(join(protocol, "src/current.ts"), "utf8")).not.toBe(current);
    write("desktop/packages/protocol/src/gen/serial/v1/cordial_pb.ts", "stale");
    expect(() => generate("--check")).toThrow();
    generate();
    expect(existsSync(join(protocol, "src/gen/serial/v1"))).toBe(false);
    mkdirSync(join(protocol, "src/gen/serial/v1"));
    expect(() => generate("--check")).toThrow();
    generate();
    expect(existsSync(join(protocol, "src/gen/serial/v1"))).toBe(false);
    generate("--check");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}, 20_000);
