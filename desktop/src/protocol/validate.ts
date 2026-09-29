// Validates messages against the checked-in wire schema. Responses are
// checked against the definition of the command that started them.
import type { CommandName } from "./types.ts";
import { validator } from "./validators.ts";

function check(definition: string, message: unknown): string | null {
  const validate = validator(definition);
  if (validate(message)) return null;
  const first = validate.errors?.[0];
  return first ? `${first.instancePath || "/"} ${first.message ?? "is invalid"}` : "invalid";
}

/** Null when `message` is a valid response to `command`, else the problem. */
export const responseProblem = (command: CommandName, message: unknown) =>
  check(`${command}.response`, message);
/** Null when `message` is a valid event, else the problem. */
export const eventProblem = (message: unknown) => check("Event", message);
/** Null when `message` is a valid request, else the problem. */
export const requestProblem = (command: CommandName, message: unknown) =>
  check(`${command}.request`, message);
