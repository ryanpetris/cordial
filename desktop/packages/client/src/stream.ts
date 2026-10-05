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
   * The USB serial number as the port reports it; empty when it reports none, and null when the
   * host can't read it (Web Serial). Only the status of an open session identifies the adapter.
   */
  serial: string | null;
}

/** The characters of the adapter ID that begin every Dongle's USB serial number. */
const ADAPTER_ID_LENGTH = 16;

/**
 * Whether a port's USB serial number begins with the adapter ID `id`, compared without regard to
 * case, so an adapter can be recognized before its port is opened. Anything after the ID, such as
 * the marker Vial looks for, is ignored.
 */
export const serialMatches = (serial: string, id: string) => serial.slice(0, ADAPTER_ID_LENGTH).toUpperCase() === id.toUpperCase();
