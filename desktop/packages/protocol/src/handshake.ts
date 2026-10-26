import { fromBinary, toBinary } from "@bufbuild/protobuf";
import { ClientHelloSchema, AdapterHelloSchema } from "./gen/handshake_pb.ts";
import type { ClientHello, AdapterHello } from "./gen/handshake_pb.ts";
import { MAX_REQUEST_BYTES } from "./limits.ts";
export * from "./gen/handshake_pb.ts";
export { CURRENT_PROTOCOL_VERSION } from "./current.ts";

/** Maximum decoded protobuf size of either hello, before COBS encoding. */
export const MAX_HANDSHAKE_BYTES = MAX_REQUEST_BYTES;
export type HandshakeErrorCode = "too_long" | "missing_version" | "zero_version" | "duplicate_version" | "invalid_version" | "malformed";

/** A technical validation result for the host to present in its own context. */
export class HandshakeError extends Error {
  constructor(readonly code: HandshakeErrorCode) {
    super(code);
    this.name = "HandshakeError";
  }
}

function validate(bytes: Uint8Array): void {
  const fail = (code: HandshakeErrorCode): never => { throw new HandshakeError(code); };
  if (bytes.length > MAX_HANDSHAKE_BYTES) fail("too_long");
  let cursor = 0;
  let version: bigint | undefined;
  let invalidMetadata = false;
  function varint(code: HandshakeErrorCode = "malformed"): bigint {
    let value = 0n;
    for (let i = 0; i < 10; i++) {
      const byte = bytes[cursor++];
      if (byte === undefined || (i === 9 && byte > 1)) return fail(code);
      value |= BigInt(byte & 0x7f) << BigInt(i * 7);
      if (byte < 0x80) return value;
    }
    return fail(code);
  }
  function skip(length: bigint): void {
    if (length > BigInt(bytes.length - cursor)) fail("malformed");
    cursor += Number(length);
  }
  function fields(group = 0, depth = 0): void {
    if (depth > 100) fail("malformed");
    while (cursor < bytes.length) {
      const start = cursor;
      const key = varint();
      if (cursor - start > 5 || key > 0xffffffffn || key < 8n) fail("malformed");
      const tag = Number(key >> 3n);
      const wire = Number(key & 7n);
      if (wire > 5) fail("malformed");
      if (group === 0 && tag === 1) {
        if (version !== undefined) fail("duplicate_version");
        if (wire !== 0) fail("invalid_version");
        version = varint("invalid_version");
        if (version > 0xffffffffn) fail("invalid_version");
        if (version === 0n) fail("zero_version");
        continue;
      }
      if (wire === 4) {
        if (group !== tag) fail("malformed");
        return;
      }
      // Prost permits an empty group at depth 100, but no fields within it.
      if (depth === 100) fail("malformed");
      if (group === 0 && tag >= 2 && tag <= 4 && wire !== 2) invalidMetadata = true;
      switch (wire) {
        case 0: varint(); break;
        case 1: skip(8n); break;
        case 2: skip(varint()); break;
        case 3: fields(tag, depth + 1); break;
        case 5: skip(4n); break;
        default: fail("malformed");
      }
    }
    if (group !== 0) fail("malformed");
  }
  fields();
  if (version === undefined) fail("missing_version");
  if (invalidMetadata) fail("malformed");
}

/** Decodes the fixed empty identification query. Nonempty bytes are not a host hello. */
export function decodeClientHello(bytes: Uint8Array): ClientHello {
  if (bytes.length > MAX_HANDSHAKE_BYTES) throw new HandshakeError("too_long");
  if (bytes.length !== 0) throw new HandshakeError("malformed");
  return fromBinary(ClientHelloSchema, bytes);
}

/** Validates and decodes an adapter hello. All metadata is optional. */
export function decodeAdapterHello(bytes: Uint8Array): AdapterHello {
  validate(bytes);
  try { return fromBinary(AdapterHelloSchema, bytes, { recursionLimit: 101 }); }
  catch { throw new HandshakeError("malformed"); }
}

function validateVersion(version: number | undefined): void {
  if (version === undefined) throw new HandshakeError("missing_version");
  if (version === 0) throw new HandshakeError("zero_version");
  if (!Number.isInteger(version) || version < 0 || version > 0xffffffff) throw new HandshakeError("invalid_version");
}

/** Encodes the fixed empty identification query. Pass the bytes to encodeFrame. */
export function encodeClientHello(hello: ClientHello): Uint8Array {
  const bytes = toBinary(ClientHelloSchema, hello);
  decodeClientHello(bytes);
  return bytes;
}

/** Encodes and validates an adapter hello, including the metadata's total size. */
export function encodeAdapterHello(hello: AdapterHello): Uint8Array {
  validateVersion(hello.protocolVersion);
  for (const value of [hello.firmwareVersion, hello.serialNumber, hello.name]) {
    if (value !== undefined && value.length > MAX_HANDSHAKE_BYTES) throw new HandshakeError("too_long");
  }
  const bytes = toBinary(AdapterHelloSchema, hello);
  validate(bytes);
  return bytes;
}
