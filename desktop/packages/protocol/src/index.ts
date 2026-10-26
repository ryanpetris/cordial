// The Cordial serial API: protobuf messages generated from
// proto/serial/v1/cordial.proto and the frame encoding used on the USB serial port.
export * from "./gen/cordial_pb.ts";
export * from "./gen/migration_pb.ts";
export * as storageV1 from "./gen/storage/v1/storage_pb.ts";
export * as storageV2 from "./gen/storage/v2/storage_pb.ts";
export * from "./framing.ts";
export * from "./handshake.ts";
export * from "./current.ts";
export * from "./limits.ts";

/** USB vendor ID of every Dongle (pid.codes). */
export const USB_VENDOR_ID = 0x1209;
/** USB product ID of every Dongle. */
export const USB_PRODUCT_ID = 0xc0d1;

/** Maximum protobuf IPC payload length after its four-byte little-endian prefix. */
export const MIGRATION_MAX_FRAME_BYTES = 65_536;
/** Conservative migration data chunk length, independent of the serial envelope. */
export const STORAGE_MIGRATION_CHUNK_BYTES = 512;
/** Maximum UTF-8 byte length of an absolute migration dataset path. */
export const STORAGE_MIGRATION_PATH_BYTES = 192;
/** Maximum inventory file count accepted by a host before downloading or allocating. */
export const STORAGE_MIGRATION_MAX_FILES = 16_384;
/** Maximum checked total input bytes accepted by a host, independent of adapter capacity. */
export const STORAGE_MIGRATION_MAX_INPUT_BYTES = 64 * 1024 * 1024;
/** Host RAM budget for migration input, output and working allocations together. */
export const STORAGE_MIGRATION_MAX_MEMORY_BYTES = 256 * 1024 * 1024;
