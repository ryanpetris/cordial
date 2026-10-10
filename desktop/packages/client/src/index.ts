// A client for the Cordial serial API over any byte stream. The `node` and
// `web` entries add serial port discovery and streams for Node and Web Serial.
export { Connection, ConnectionClosedError, CordialError, UnexpectedResponseError, entryId, readAll, type Command, type ConnectionOptions, type Page } from "./connection.ts";
export * from "./order.ts";
export { serialMatches, type ByteStream, type PortInfo } from "./stream.ts";
