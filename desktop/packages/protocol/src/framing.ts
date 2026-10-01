// Frames on the serial port: each message is protobuf-encoded, then
// COBS-encoded, then followed by one zero byte. COBS output contains no zero
// byte, so a zero always ends a frame and a receiver that starts mid-stream
// resynchronizes at the next one.

/** The frame delimiter. A client sends one before its first request so that
 * bytes left by an earlier client cannot merge with it. */
export const DELIMITER = 0;

/** The COBS encoding of `bytes` followed by the delimiter. */
export function encodeFrame(bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(bytes.length + Math.floor(bytes.length / 254) + 2);
  let codeAt = 0;
  let length = 1;
  let code = 1;
  for (const byte of bytes) {
    if (byte === 0) {
      out[codeAt] = code;
      codeAt = length++;
      code = 1;
      continue;
    }
    out[length++] = byte;
    if (++code === 0xff) {
      out[codeAt] = code;
      codeAt = length++;
      code = 1;
    }
  }
  out[codeAt] = code;
  out[length++] = DELIMITER;
  return out.slice(0, length);
}

/** Decodes COBS bytes without their delimiter; null when they are not valid COBS. */
export function decodeCobs(bytes: Uint8Array): Uint8Array | null {
  const out = new Uint8Array(bytes.length);
  let read = 0;
  let write = 0;
  while (read < bytes.length) {
    const code = bytes[read]!;
    if (code === 0 || read + code > bytes.length) return null;
    read++;
    for (let i = 1; i < code; i++) out[write++] = bytes[read++]!;
    if (code < 0xff && read < bytes.length) out[write++] = 0;
  }
  return out.slice(0, write);
}

/** A decoded frame, or why the bytes up to a delimiter could not be used. */
export type Frame = { ok: true; bytes: Uint8Array } | { ok: false; error: "too_long" | "malformed" };

/** Collects received bytes into frames. Empty frames are skipped. */
export class FrameDecoder {
  readonly #limit: number | null;
  #chunks: Uint8Array[] = [];
  #length = 0;
  #discarding = false;

  /** A decoder that rejects frames longer than `limit` bytes once decoded,
   * or accepts any length when `limit` is null. */
  constructor(limit: number | null = null) {
    this.#limit = limit;
  }

  /** The most encoded bytes a frame within the limit can take. */
  get #encodedLimit() {
    return this.#limit === null ? Infinity : this.#limit + Math.floor(this.#limit / 254) + 1;
  }

  /** Drops any partial frame, as at the start of a new session. */
  reset() {
    this.#chunks = [];
    this.#length = 0;
    this.#discarding = false;
  }

  /** Adds received bytes and returns every frame they complete. */
  push(chunk: Uint8Array): Frame[] {
    const frames: Frame[] = [];
    let start = 0;
    for (;;) {
      const end = chunk.indexOf(DELIMITER, start);
      const part = chunk.subarray(start, end === -1 ? chunk.length : end);
      if (!this.#discarding && part.length) {
        if (this.#length + part.length > this.#encodedLimit) {
          this.#discarding = true;
          this.#chunks = [];
          this.#length = 0;
        } else {
          this.#chunks.push(part.slice());
          this.#length += part.length;
        }
      }
      if (end === -1) return frames;
      start = end + 1;
      const frame = this.#finish();
      if (frame) frames.push(frame);
    }
  }

  #finish(): Frame | null {
    const discarding = this.#discarding;
    const encoded = this.#chunks.length === 1 ? this.#chunks[0]! : concat(this.#chunks, this.#length);
    this.reset();
    if (discarding) return { ok: false, error: "too_long" };
    if (!encoded.length) return null;
    const bytes = decodeCobs(encoded);
    if (!bytes) return { ok: false, error: "malformed" };
    if (this.#limit !== null && bytes.length > this.#limit) return { ok: false, error: "too_long" };
    return { ok: true, bytes };
  }
}

function concat(chunks: Uint8Array[], length: number): Uint8Array {
  const out = new Uint8Array(length);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}
