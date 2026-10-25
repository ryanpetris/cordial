// Whether this app can use an adapter's saved data, decided from the status that opens each
// session and kept current by adapter events. Only an adapter whose firmware operates on the
// storage format this app uses takes requests beyond that status. Firmware that starts in assisted
// mode reports STORAGE_MIGRATION_REQUIRED until it restarts; the Cordial CLI then finds the adapter's source
// format from its files and migrates them. The reported storage format plays no part here.
import { ErrorCode, ErrorOutcome, ErrorReason, type MigrationControlSchema, type MigrationEvent, type Status } from "@cordial/protocol";
import type { MessageInitShape } from "@bufbuild/protobuf";
import type { WireError } from "../shared/state.ts";
import { wireError } from "./convert.ts";

/** The storage format this app operates on. */
export const STORAGE_FORMAT = 2;

export type StorageGate =
  /** The adapter isn't in assisted mode. It takes requests once its status is ready. */
  | { kind: "current" }
  /** The CLI must migrate the adapter's saved data. */
  | { kind: "assisted" }
  /** The adapter is in assisted mode, but `error` stops the CLI from migrating it. */
  | { kind: "blocked"; error: WireError }
  /** The adapter's firmware operates on another storage format. */
  | { kind: "unsupported" }
  /** The adapter's firmware doesn't report the storage format it operates on. */
  | { kind: "unknown" };

/** The error assisted mode reports, exactly. */
const assisted = (e: Status["errors"][number]) =>
  e.code === ErrorCode.STORAGE_MIGRATION_REQUIRED && e.reason === ErrorReason.STORAGE && e.outcome === ErrorOutcome.NO_EFFECT && !e.related.length;

export function storageGate({ information, errors }: Status): StorageGate {
  const target = information?.targetStorageFormat ?? 0;
  if (!target) return { kind: "unknown" };
  if (target !== STORAGE_FORMAT) return { kind: "unsupported" };
  if (!errors.some((e) => e.code === ErrorCode.STORAGE_MIGRATION_REQUIRED)) return { kind: "current" };
  // Any other error, or the marker with other details, stops migration before the CLI starts.
  const blocker = errors.find((e) => !assisted(e));
  return blocker ? { kind: "blocked", error: wireError(blocker) } : { kind: "assisted" };
}

export interface MigrationHandlers {
  event(event: MigrationEvent): void;
  /** The CLI's output broke the framing or held an undecodable message; nothing after it is read. */
  malformed(reason: string): void;
  /** The CLI exited and its output closed, or it never started; `code` is null without an exit code. */
  exit(code: number | null, signal: string | null): void;
}

export interface MigrationProcess {
  send(control: MessageInitShape<typeof MigrationControlSchema>): void;
}

/** Runs `cordial migrate --ipc`; only hosts that can start processes provide it. */
export interface MigrationHost {
  /** The CLI's path, found without starting it; null when it isn't installed. */
  find(): string | null;
  /** Starts the CLI at `path`. Errors that stop it from starting arrive as `exit`. */
  start(path: string, handlers: MigrationHandlers): MigrationProcess;
}
