import { describe, expect, it } from "vitest";
import { FrameDecoder, decodeCobs, encodeFrame } from "../src/index.ts";

const bytes = (...b: number[]) => new Uint8Array(b);

describe("framing", () => {
  it("encodes zeros and long runs without a zero byte before the delimiter", () => {
    expect([...encodeFrame(bytes())]).toEqual([1, 0]);
    expect([...encodeFrame(bytes(0))]).toEqual([1, 1, 0]);
    expect([...encodeFrame(bytes(0x11, 0, 0x22))]).toEqual([2, 0x11, 2, 0x22, 0]);
    const long = new Uint8Array(600).map((_, i) => (i % 255) + 1);
    const frame = encodeFrame(long);
    expect(frame.subarray(0, -1).includes(0)).toBe(false);
    expect(frame.at(-1)).toBe(0);
    expect([...decodeCobs(frame.subarray(0, -1))!]).toEqual([...long]);
  });

  it("round-trips every length around a code block boundary", () => {
    for (const length of [0, 1, 253, 254, 255, 256, 508, 509]) {
      const data = new Uint8Array(length).map((_, i) => (i * 7) % 256);
      const decoded = new FrameDecoder().push(encodeFrame(data));
      expect(decoded).toEqual([{ ok: true, bytes: data }]);
    }
  });

  it("joins pieces, splits several frames and skips empty ones", () => {
    const decoder = new FrameDecoder();
    const a = encodeFrame(bytes(1, 2, 3));
    const b = encodeFrame(bytes(0, 4));
    expect(decoder.push(a.subarray(0, 2))).toEqual([]);
    expect(decoder.push(bytes(...a.subarray(2), 0, 0, ...b))).toEqual([
      { ok: true, bytes: bytes(1, 2, 3) },
      { ok: true, bytes: bytes(0, 4) },
    ]);
  });

  it("rejects frames over the limit through their delimiter and resynchronizes", () => {
    const decoder = new FrameDecoder(4);
    const big = encodeFrame(bytes(1, 2, 3, 4, 5, 6, 7, 8, 9));
    expect(decoder.push(bytes(...big, ...encodeFrame(bytes(9))))).toEqual([
      { ok: false, error: "too_long" },
      { ok: true, bytes: bytes(9) },
    ]);
    // Five bytes encode in six, within the encoded allowance, and are still too long.
    expect(decoder.push(encodeFrame(bytes(1, 2, 3, 4, 5)))).toEqual([{ ok: false, error: "too_long" }]);
    expect(decoder.push(encodeFrame(bytes(1, 2, 3, 4)))).toEqual([{ ok: true, bytes: bytes(1, 2, 3, 4) }]);
  });

  it("reports malformed COBS and drops a partial frame on reset", () => {
    const decoder = new FrameDecoder();
    expect(decoder.push(bytes(5, 1, 0))).toEqual([{ ok: false, error: "malformed" }]);
    decoder.push(bytes(3, 1));
    decoder.reset();
    expect(decoder.push(encodeFrame(bytes(7)))).toEqual([{ ok: true, bytes: bytes(7) }]);
  });
});
