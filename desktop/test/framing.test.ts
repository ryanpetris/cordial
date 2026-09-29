import { describe, expect, it } from "vitest";
import { LineDecoder } from "../src/protocol/framing.ts";

const bytes = (s: string) => new TextEncoder().encode(s);

describe("LineDecoder", () => {
  it("joins pieces, splits several lines and skips empty ones", () => {
    const d = new LineDecoder();
    expect(d.push(bytes('{"a":'))).toEqual([]);
    expect(d.push(bytes('1}\n\n{"b":2}\r\n{"c"'))).toEqual([{ text: '{"a":1}' }, { text: '{"b":2}' }]);
    expect(d.push(bytes(":3}\n"))).toEqual([{ text: '{"c":3}' }]);
  });

  it("discards an oversized line through its terminator", () => {
    const d = new LineDecoder();
    // The 4,096-byte limit includes the LF.
    expect(d.push(bytes("x".repeat(4096)))).toEqual([{ error: "oversized" }]);
    expect(d.push(bytes("yyy\nok\n"))).toEqual([{ text: "ok" }]);
    expect(new LineDecoder().push(bytes(`${"x".repeat(4096)}\n`))).toEqual([{ error: "oversized" }]);
    expect(new LineDecoder().push(bytes(`${"x".repeat(4095)}\n`))).toEqual([{ text: "x".repeat(4095) }]);
  });

  it("rejects invalid UTF-8", () => {
    expect(new LineDecoder().push(new Uint8Array([0xff, 0x0a]))).toEqual([{ error: "invalid_utf8" }]);
  });
});
