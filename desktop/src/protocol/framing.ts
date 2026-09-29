// Newline framing of the serial stream. USB transfers are not message
// boundaries; a line may arrive in pieces and a chunk may hold several lines.
import { MAX_LINE_BYTES } from "./types.ts";

export type Line = { text: string } | { error: "oversized" | "invalid_utf8" };

export class LineDecoder {
  #parts: Uint8Array[] = [];
  #length = 0;
  #discarding = false;
  readonly #utf8 = new TextDecoder("utf-8", { fatal: true });

  /** Splits received bytes into complete lines, skipping empty ones. */
  push(chunk: Uint8Array): Line[] {
    const lines: Line[] = [];
    let start = 0;
    for (;;) {
      const end = chunk.indexOf(0x0a, start);
      const piece = chunk.subarray(start, end === -1 ? chunk.length : end);
      if (!this.#discarding) {
        // The limit includes the LF, so content may use MAX_LINE_BYTES - 1.
        if (this.#length + piece.length > MAX_LINE_BYTES - 1) {
          this.#discarding = true;
          this.#parts = [];
          this.#length = 0;
          lines.push({ error: "oversized" });
        } else if (piece.length) {
          this.#parts.push(piece.slice());
          this.#length += piece.length;
        }
      }
      if (end === -1) break;
      if (!this.#discarding && this.#length) {
        const line = this.#finish();
        // A lone CR is an empty CRLF line.
        if (!("text" in line) || line.text) lines.push(line);
      }
      this.#discarding = false;
      this.#parts = [];
      this.#length = 0;
      start = end + 1;
    }
    return lines;
  }

  #finish(): Line {
    const bytes = new Uint8Array(this.#length);
    let offset = 0;
    for (const part of this.#parts) {
      bytes.set(part, offset);
      offset += part.length;
    }
    let text: string;
    try {
      text = this.#utf8.decode(bytes);
    } catch {
      return { error: "invalid_utf8" };
    }
    if (text.endsWith("\r")) text = text.slice(0, -1);
    return { text };
  }

  /** Forgets any partial line. */
  reset(): void {
    this.#parts = [];
    this.#length = 0;
    this.#discarding = false;
  }
}
