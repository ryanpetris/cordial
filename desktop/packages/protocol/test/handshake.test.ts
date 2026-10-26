import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import fixtures from "../../../../rust/crates/cordial-protocol/tests/fixtures/handshake.json";
import {
  AdapterHelloSchema, ClientHelloSchema, CURRENT_PROTOCOL_VERSION, MAX_HANDSHAKE_BYTES,
  decodeClientHello, decodeAdapterHello, encodeClientHello, encodeAdapterHello,
  HandshakeError, FrameDecoder, RequestSchema, encodeFrame,
} from "../src/index.ts";

const bytes = (...values: number[]) => new Uint8Array(values);

describe("permanent handshake", () => {
  it("matches the shared accepted and rejected wire fixtures", () => {
    for (const fixture of fixtures) {
      for (const [decode, error] of [
        [decodeClientHello, fixture.client_error],
        [decodeAdapterHello, fixture.adapter_error],
      ] as const) {
        if (error) {
          expect(() => decode(new Uint8Array(fixture.bytes)), JSON.stringify(fixture))
            .toThrow(expect.objectContaining({ code: error }));
        } else {
          expect(decode(new Uint8Array(fixture.bytes)), JSON.stringify(fixture))
            .toEqual(decode === decodeClientHello ? create(ClientHelloSchema) : expect.objectContaining({ protocolVersion: 1 }));
        }
      }
    }
  });

  it("encodes an empty query and distinguishes it from bare delimiters and requests", () => {
    const client = create(ClientHelloSchema);
    expect(encodeClientHello(client)).toEqual(bytes());
    expect(decodeClientHello(encodeClientHello(client))).toEqual(client);
    const frame = encodeFrame(encodeClientHello(client));
    expect(frame).toEqual(bytes(1, 0));
    const decoder = new FrameDecoder(MAX_HANDSHAKE_BYTES);
    expect(decoder.push(bytes(0, 0, frame[0]!))).toEqual([]);
    expect(decoder.push(frame.subarray(1))).toEqual([{ ok: true, bytes: bytes() }]);
    const request = toBinary(RequestSchema, create(RequestSchema, { command: { case: "getStatus", value: {} } }));
    expect(request.length).toBeGreaterThan(0);
    expect(() => decodeClientHello(request)).toThrow(expect.objectContaining({ code: "malformed" }));
    expect(decoder.push(bytes(0, ...encodeFrame(request), 0, ...frame, 0))).toEqual([
      { ok: true, bytes: request },
      { ok: true, bytes: bytes() },
    ]);
    const unknown = fromBinary(ClientHelloSchema, bytes(8, 1));
    expect(() => encodeClientHello(unknown)).toThrow(expect.objectContaining({ code: "malformed" }));
  });

  it("accepts noncurrent versions and absent or independently present metadata", () => {
    for (const protocolVersion of [CURRENT_PROTOCOL_VERSION, 2, 0xffffffff]) {
      const adapter = create(AdapterHelloSchema, { protocolVersion });
      expect(decodeAdapterHello(encodeAdapterHello(adapter))).toEqual(adapter);
    }
    const adapter = create(AdapterHelloSchema, { protocolVersion: 1, firmwareVersion: "0.0.0", serialNumber: "example", name: "" });
    expect(decodeAdapterHello(encodeAdapterHello(adapter))).toEqual(adapter);
    const reordered = decodeAdapterHello(bytes(34, 1, 65, 26, 1, 66, 18, 1, 67, 8, 1));
    expect(reordered).toMatchObject({ protocolVersion: 1, name: "A", serialNumber: "B", firmwareVersion: "C" });
  });

  it("skips future fields in either order without reading nested versions", () => {
    const unknown = [42, 2, 8, 99, 48, 2, 61, 0, 0, 0, 0, 65, 0, 0, 0, 0, 0, 0, 0, 0, 75, 8, 99, 76];
    for (const data of [bytes(...unknown, 8, 1), bytes(8, 1, ...unknown)]) {
      expect(decodeAdapterHello(data).protocolVersion).toBe(1);
    }
    const nested = bytes(8, 1, ...Array(101).fill(43), ...Array(101).fill(44));
    expect(() => decodeAdapterHello(nested)).toThrow(HandshakeError);
  });

  it("checks adapter metadata wire types and UTF-8", () => {
    for (const data of [bytes(8, 1, 18, 1, 255), bytes(8, 1, 16, 1)]) {
      expect(() => decodeAdapterHello(data)).toThrow(expect.objectContaining({ code: "malformed" }));
    }
  });

  it("bounds encoded and decoded hellos and reuses the COBS frame limit", () => {
    const adapter = create(AdapterHelloSchema, { protocolVersion: 1, name: "a".repeat(MAX_HANDSHAKE_BYTES - 5) });
    const data = encodeAdapterHello(adapter);
    expect(data.length).toBe(MAX_HANDSHAKE_BYTES);
    expect(decodeAdapterHello(data).protocolVersion).toBe(1);
    const frames = new FrameDecoder(MAX_HANDSHAKE_BYTES);
    expect(frames.push(encodeFrame(data))).toEqual([{ ok: true, bytes: data }]);
    adapter.name += "a";
    expect(() => encodeAdapterHello(adapter)).toThrow(expect.objectContaining({ code: "too_long" }));
    const overlong = toBinary(AdapterHelloSchema, adapter);
    for (const decode of [decodeAdapterHello, decodeClientHello]) {
      expect(() => decode(overlong)).toThrow(expect.objectContaining({ code: "too_long" }));
    }
    expect(frames.push(encodeFrame(overlong))).toEqual([{ ok: false, error: "too_long" }]);
    expect(() => encodeAdapterHello(create(AdapterHelloSchema))).toThrow(expect.objectContaining({ code: "missing_version" }));
    expect(() => encodeAdapterHello(create(AdapterHelloSchema, { protocolVersion: 0 }))).toThrow(expect.objectContaining({ code: "zero_version" }));
    for (const protocolVersion of [-1, 0x100000000, NaN, 1.5]) {
      expect(() => encodeAdapterHello(create(AdapterHelloSchema, { protocolVersion }))).toThrow(expect.objectContaining({ code: "invalid_version" }));
    }
  });
});
