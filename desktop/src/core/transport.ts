// Serial byte streams. The session only needs bytes and close notification,
// so each host supplies its own: the serial library in Node, Web Serial in a
// browser, and in-memory streams for tests and the simulated adapter.

/** An open port whose DTR is raised. */
export interface Transport {
  /** Receives every chunk of bytes read from the port. */
  onData(listener: (chunk: Uint8Array) => void): void;
  /** Called once when the port closes or fails, never after close(). */
  onClose(listener: (error: Error | null) => void): void;
  write(text: string): Promise<void>;
  /** Closes the port, lowering DTR. */
  close(): Promise<void>;
}

export interface PortInfo {
  path: string;
  /**
   * USB serial number; Cordial uses its adapter ID. Null when the host can't
   * read it (Web Serial): the handshake then identifies the adapter.
   */
  serial: string | null;
}

/** Cordial's USB vendor and product IDs. */
export const USB_VENDOR_ID = 0xcafe;
export const USB_PRODUCT_ID = 0x4014;
