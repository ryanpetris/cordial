// Typed views of the generated wire contract.
import type { Device, Event, Request, ResponseMap, WireError } from "./wire.ts";

export type * from "./wire.ts";

export type ProtocolState = NonNullable<Device["hidpp_protocol"]>;

export type CommandName = Request["cmd"];
export type ArgsOf<C extends CommandName> = Extract<Request, { cmd: C }>["args"];
type ResponseOf<C extends CommandName> = ResponseMap[C];
export type ResultOf<C extends CommandName> = Extract<ResponseOf<C>, { ok: true; done: true }>["result"];
export type ChunkOf<C extends CommandName> = Extract<ResponseOf<C>, { ok: true; done: false }>["result"];
export type EventName = Event["event"];
export type EventOf<E extends EventName> = Extract<Event, { event: E }>;

/** A terminal error response from the adapter. */
export class AdapterError extends Error {
  readonly wire: WireError;
  readonly command: CommandName;
  constructor(command: CommandName, wire: WireError) {
    super(`${command} failed: ${wire.code}`);
    this.name = "AdapterError";
    this.command = command;
    this.wire = wire;
  }
}

/** The control session ended or failed before a request finished. */
export class SessionClosedError extends Error {
  constructor(message = "control session closed") {
    super(message);
    this.name = "SessionClosedError";
  }
}

/** Version 1 limits and timing from docs/protocol/. */
export const MAX_LINE_BYTES = 4096;
export const HEARTBEAT_INTERVAL_MS = 5000;
export const MAX_REQUEST_ID = 2_147_483_647;
