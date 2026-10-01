// Byte streams to a Dongle. A Connection only needs bytes and close
// notification, so each host supplies its own: a Node serial port, Web Serial
// in a browser, or an in-memory stream for tests and simulation.

/** An open port whose DTR is raised, which starts a session on the Dongle. */
export interface ByteStream {
  /** Receives every chunk of bytes read from the port. */
  onData(listener: (chunk: Uint8Array) => void): void;
  /** Called once when the port closes or fails, never after close(). */
  onClose(listener: (error: Error | null) => void): void;
  write(bytes: Uint8Array): Promise<void>;
  /** Closes the port, lowering DTR, which ends the session. */
  close(): Promise<void>;
}

/** A serial port that may be a Dongle. */
export interface PortInfo {
  path: string;
  /**
   * USB serial number, which is the Dongle's adapter ID. Null when the host
   * can't read it (Web Serial): the Dongle's status then identifies it.
   */
  serial: string | null;
}
