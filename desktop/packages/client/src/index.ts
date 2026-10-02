// A client for the Cordial serial API over any byte stream. The `node` and
// `web` entries add serial port discovery and streams for Node and Web Serial.
export { Connection, ConnectionClosedError, CordialError, UnexpectedResponseError, type Command, type ConnectionOptions } from "./connection.ts";
export type { ByteStream, PortInfo } from "./stream.ts";
