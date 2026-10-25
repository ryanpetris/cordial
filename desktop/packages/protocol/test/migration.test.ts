import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { expect, it } from "vitest";
import {
  ErrorCode, ErrorOutcome, ErrorReason, MAX_REQUEST_BYTES, MIGRATION_MAX_FRAME_BYTES, MigrationControlSchema, MigrationEventSchema,
  RequestSchema, STORAGE_MIGRATION_CHUNK_BYTES, STORAGE_MIGRATION_PATH_BYTES,
  StatusSchema, storageV1, storageV2,
} from "../src/index.ts";

it("generates both storage schemas and preserves the v1 sequence header collision", () => {
  const root = toBinary(storageV1.FormatSchema, create(storageV1.FormatSchema, { format: 1, initialized: true }));
  const sequence = toBinary(storageV1.SequenceSchema, create(storageV1.SequenceSchema, { nextDevice: 2n, nextProfile: 1n }));
  expect([...root]).toEqual([8, 1, 16, 1]);
  expect([...sequence]).toEqual([8, 2, 16, 1]);
  expect(fromBinary(storageV1.SequenceSchema, sequence).nextDevice).toBe(2n);
  expect([...toBinary(storageV2.SequenceSchema, create(storageV2.SequenceSchema, { format: 2, nextDevice: 2n, nextProfile: 1n }))]).toEqual([8, 2, 16, 2, 24, 1]);
});

it("preserves the completed v2 sequence collision with a loose v1 decode", () => {
  const bytes = new Uint8Array([0x08, 0x02, 0x10, 0x05, 0x18, 0x07]);
  const completed = fromBinary(storageV2.SequenceSchema, bytes);
  expect(completed.format).toBe(2);
  expect(completed.nextDevice).toBe(5n);
  expect(completed.nextProfile).toBe(7n);
  const loose = fromBinary(storageV1.SequenceSchema, bytes);
  expect(loose.nextDevice).toBe(2n);
  expect(loose.nextProfile).toBe(5n);
  // Ignoring unknown outer tag 3 changes the IDs, so decode success cannot classify a file.
  expect(toBinary(storageV1.SequenceSchema, loose, { writeUnknownFields: false })).not.toEqual(bytes);
});

it("fits bounded file requests inside the serial limit", () => {
  const path = `/${"a".repeat(STORAGE_MIGRATION_PATH_BYTES - 1)}`;
  const requests = [
    create(RequestSchema, { command: { case: "listFiles", value: { path } } }),
    create(RequestSchema, { command: { case: "readFile", value: { path, offset: 0xffffffffffffffffn, length: STORAGE_MIGRATION_CHUNK_BYTES } } }),
    create(RequestSchema, { command: { case: "writeFile", value: { path, offset: 0xffffffffffffffffn, data: new Uint8Array(STORAGE_MIGRATION_CHUNK_BYTES).fill(255) } } }),
    create(RequestSchema, { command: { case: "moveFile", value: { source: path, destination: path } } }),
    create(RequestSchema, { command: { case: "deleteFile", value: { path } } }),
    create(RequestSchema, { command: { case: "restart", value: {} } }),
  ];
  for (const request of requests) {
    const bytes = toBinary(RequestSchema, request);
    expect(bytes.length).toBeLessThanOrEqual(MAX_REQUEST_BYTES);
    expect(fromBinary(RequestSchema, bytes)).toEqual(request);
  }
});

it("reports assistance and the target independently of readiness and the current format", () => {
  const marker = { code: ErrorCode.STORAGE_MIGRATION_REQUIRED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.NO_EFFECT };
  for (const storageFormat of [undefined, 1, 2])
    for (const errors of [[], [marker]]) {
      const status = create(StatusSchema, { ready: false, errors, information: { storageFormat, targetStorageFormat: 2 } });
      expect(fromBinary(StatusSchema, toBinary(StatusSchema, status))).toEqual(status);
    }
});

it("exports separate IPC controls and events with bounded length-prefix framing", () => {
  const control = create(MigrationControlSchema, { action: { case: "start", value: { adapterId: "0123456789ABCDEF" } } });
  expect(fromBinary(MigrationControlSchema, toBinary(MigrationControlSchema, control))).toEqual(control);
  const event = create(MigrationEventSchema, { event: { case: "success", value: { adapterId: "0123456789ABCDEF", targetFormat: 2 } } });
  const bytes = toBinary(MigrationEventSchema, event);
  expect(bytes.length).toBeLessThanOrEqual(MIGRATION_MAX_FRAME_BYTES);
  const frame = new Uint8Array(bytes.length + 4);
  new DataView(frame.buffer).setUint32(0, bytes.length, true);
  frame.set(bytes, 4);
  expect(new DataView(frame.buffer).getUint32(0, true)).toBe(bytes.length);
  expect(fromBinary(MigrationEventSchema, frame.subarray(4))).toEqual(event);
});
