// The Cordial serial API: protobuf messages generated from
// proto/cordial.proto, the frame encoding used on the USB serial port, and the
// information and setting keys from proto/keys.toml.
export * from "./gen/cordial_pb.ts";
export * from "./framing.ts";
export * as keys from "./keys.ts";
export type { KeyEntry, KeyType } from "./keys.ts";

/** USB vendor ID of every Dongle (pid.codes). */
export const USB_VENDOR_ID = 0x1209;
/** USB product ID of every Dongle. */
export const USB_PRODUCT_ID = 0xc0d1;
/** USB manufacturer string of every Dongle. */
export const USB_MANUFACTURER = "Cordial";
/** The longest request a Dongle accepts, before frame encoding. */
export const MAX_REQUEST_BYTES = 1024;
