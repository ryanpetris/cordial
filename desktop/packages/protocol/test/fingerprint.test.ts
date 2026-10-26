import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { clone, fromBinary } from "@bufbuild/protobuf";
import { FileDescriptorSetSchema } from "@bufbuild/protobuf/wkt";
import { expect, it } from "vitest";
import { fingerprint } from "../scripts/fingerprint.mjs";
import { OPERATIONAL_SCHEMA_SHA256, CURRENT_PROTOCOL_VERSION } from "../src/index.ts";

const root = fileURLToPath(new URL("../../../../", import.meta.url));
const fixtures = `${root}rust/crates/cordial-protocol/tests/fixtures/fingerprint`;

it("matches the operational digest checked independently by the Rust fixture", () => {
  expect(OPERATIONAL_SCHEMA_SHA256).toBe(readFileSync(`${root}proto/serial/v${CURRENT_PROTOCOL_VERSION}/schema.sha256`, "utf8").trim());
});

it("normalizes paths and comments while retaining dependency semantics", () => {
  const descriptors = fromBinary(FileDescriptorSetSchema, execFileSync(`${root}desktop/node_modules/.bin/buf`, [
    "build", ".", "--as-file-descriptor-set", "--config", `${root}desktop/packages/protocol/buf.yaml`, "-o", "-",
  ], { cwd: fixtures }));
  expect(descriptors.file).toHaveLength(3);
  const expected = readFileSync(`${fixtures}/schema.sha256`, "utf8").trim();
  expect(fingerprint(descriptors)).toBe(expected);
  const renamed = clone(FileDescriptorSetSchema, descriptors);
  for (const file of renamed.file) {
    file.name = `other/${file.name}`;
    file.dependency = file.dependency.map(name => `other/${name}`);
    if (file.sourceCodeInfo) {
      for (const location of file.sourceCodeInfo.location) {
        location.leadingComments = "Different comments and source locations.";
        location.span = [99];
      }
    }
  }
  renamed.file.reverse();
  expect(fingerprint(renamed)).toBe(expected);
  const changed = clone(FileDescriptorSetSchema, descriptors);
  changed.file[0]!.messageType[0]!.field[0]!.number = 9;
  expect(fingerprint(changed)).not.toBe(expected);
  const reordered = clone(FileDescriptorSetSchema, descriptors);
  reordered.file.find(file => file.name === "main.proto")!.dependency.reverse();
  expect(fingerprint(reordered)).not.toBe(expected);
  changed.file[0]!.messageType[0]!.field[0]!.number = 1;
  changed.file[0]!.messageType[0]!.field[0]!.type = 13;
  expect(fingerprint(changed)).not.toBe(expected);
});
