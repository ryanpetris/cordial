import { ErrorCode, ErrorOutcome, ErrorReason } from "@cordial/protocol";
import { ConnectionClosedError, CordialError } from "@cordial/client";
import { describe, expect, it } from "vitest";
import { failure } from "../src/core/session.ts";
import type { WireError } from "../src/shared/state.ts";
import { READ_FAILED, ROLES, asSentence, codeText, displayKind, errorText, kindText, profileErrorText, reasonText, stateText } from "../src/shared/text.ts";

describe("error wording", () => {
  it("shows messages as sentences and inserts them as reasons without the period", () => {
    expect(codeText("busy")).toBe("The adapter is busy. Try again when the current operation finishes.");
    expect(errorText(error({ code: "no_capacity", reason: "enabled_devices" }))).toBe(
      "Every enabled-device place is in use. Turn off another device first.",
    );
    expect(reasonText(codeText("transport_error"))).toBe("The adapter couldn't exchange messages with this device");
    expect(asSentence("couldn't open the port")).toBe("Couldn't open the port.");
    expect(asSentence("Done.")).toBe("Done.");
  });

  it("words a failed read of saved data as a read, and a failed save as a save", () => {
    const storage = new CordialError("listProfiles", { code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE });
    expect(failure(storage, true)).toBe(READ_FAILED);
    // Only an outcome of no effect confirms the save failed.
    expect(failure(storage)).toBe("The adapter couldn't read or write its saved data.");
    expect(failure(new CordialError("setSettings", { code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.NO_EFFECT }))).toBe(
      "The adapter couldn't save the change.",
    );
    expect(failure(new ConnectionClosedError(), true)).toBe("The adapter disconnected before it answered.");
    expect(failure(new ConnectionClosedError())).toBe("The adapter couldn't confirm whether the change was saved.");
  });

  it("reports an unknown outcome as unknown whatever the code, and keeps related failures", () => {
    const unknown = new CordialError("setSettings", { code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.UNKNOWN });
    expect(failure(unknown)).toBe("The adapter couldn't read or write its saved data. The adapter couldn't confirm whether the change was saved. Check before trying again.");
    expect(errorText(error({ code: "timeout", outcome: "unknown" }))).toBe(
      "This device didn't respond in time. The adapter couldn't confirm whether the change was saved. Check before trying again.",
    );
    expect(errorText(error({ code: "timeout", outcome: "unknown" }), "apply")).toBe("This device didn't respond in time. The adapter couldn't confirm whether the change took effect.");
    // Details this app doesn't know add nothing.
    expect(errorText(error({ code: 99, reason: 98, outcome: 97 }))).toBe("The adapter hit an unexpected failure.");
    expect(errorText(error({ code: "unknown" }))).toBe("The adapter hit an unexpected failure.");
  });

  it("words a related failure by its own scope, never as the primary failure's outcome", () => {
    const cleanup = error({ code: "storage_failed", reason: "storage", outcome: "unknown" });
    expect(errorText(error({ code: "auth_failed", outcome: "no_effect", related: [cleanup] }), "pair")).toBe(
      "Bluetooth authentication with this device failed. Also, the adapter couldn't read or write its saved data. The adapter couldn't confirm whether it finished cleaning up.",
    );
    expect(errorText(error({ code: "timeout", outcome: "unknown", related: [error({ code: "internal", outcome: "partial" })] }))).toBe(
      "This device didn't respond in time. The adapter couldn't confirm whether the change was saved. Check before trying again. Also, the adapter hit an unexpected failure. It partly completed.",
    );
  });

  it("never says a save failed when its outcome says it took effect", () => {
    expect(errorText(error({ code: "storage_failed", reason: "storage", outcome: "complete" }), "pair")).toBe(
      "The adapter couldn't read or write its saved data. The adapter saved the device despite this.",
    );
    expect(errorText(error({ code: "storage_failed", reason: "storage", outcome: "complete" }))).toBe(
      "The adapter couldn't read or write its saved data. The adapter saved the change despite this.",
    );
    expect(errorText(error({ code: "storage_failed", reason: "storage", outcome: "partial" }))).toBe(
      "The adapter couldn't read or write its saved data. The adapter confirmed saving only part of the change.",
    );
    expect(errorText(error({ code: "storage_failed", reason: "storage", outcome: "no_effect" }), "pair")).toBe("The adapter couldn't save the device.");
  });

  it("words a code by what it means and a reason as its context", () => {
    expect(errorText(error({ code: "not_ready", reason: "radio" }))).toBe("The adapter's Bluetooth radio isn't ready. Try again in a moment.");
    expect(errorText(error({ code: "internal", reason: "radio", related: [error({ code: "no_capacity", reason: "scan_candidates", outcome: "partial" })] }), "read")).toBe(
      "The adapter hit an unexpected failure. This involves the adapter's Bluetooth radio. Also, the adapter had no room to list every device it found. The adapter returned only some of the results.",
    );
    expect(errorText(error({ code: "usb_required_capacity_exceeded", reason: "output_backend" }))).toBe(
      "The adapter doesn't have enough USB capacity for every required interface.",
    );
    expect(errorText(error({ code: "no_capacity", reason: "output_backend" }))).toBe("The adapter doesn't have enough USB capacity for this output.");
    expect(errorText(error({ code: "timeout", reason: 99 }))).toBe("This device didn't respond in time.");
  });

  it("keeps the short Unsupported wording to plain refusals", () => {
    expect(stateText(null, error({ code: "unsupported", reason: "output_backend" }))).toBe("Unsupported");
    expect(stateText(null, error({ code: "unsupported", outcome: "no_effect" }))).toBe("Unsupported");
    expect(stateText(null, error({ code: "unsupported", outcome: "unknown" }))).toBe(
      "Failed: The adapter or device doesn't support this action. The adapter couldn't confirm whether the change took effect",
    );
    expect(stateText(null, error({ code: "unsupported", related: [error({ code: "timeout" })] }))).toBe(
      "Failed: The adapter or device doesn't support this action. Also, this device didn't respond in time",
    );
  });

  it("keeps the short lack-of-room wording to plain profile memory errors", () => {
    const short = "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles.";
    expect(profileErrorText(error({ code: "profile_memory_exceeded", reason: "profile_memory" }))).toBe(short);
    expect(profileErrorText(error({ code: "profile_memory_exceeded", reason: "profile_memory", outcome: "unknown" }))).toBe(
      "The adapter doesn't have room for this device's profiles. The adapter couldn't confirm whether the change took effect.",
    );
    expect(profileErrorText(error({ code: "profile_memory_exceeded", reason: "storage" }))).not.toBe(short);
  });

  it("names gamepads by kind, or by role without a kind", () => {
    expect(kindText(displayKind(["gamepad"]))).toBe("Gamepad");
    expect(displayKind([], ["gamepad"])).toBe("gamepad");
    expect(displayKind(["keyboard", "gamepad"])).toBe("keyboard");
    expect(ROLES.gamepad).toBe("Gamepad");
  });
});

function error(e: Partial<WireError> & Pick<WireError, "code">): WireError {
  return { reason: "unknown", outcome: "unspecified", related: [], ...e };
}
