import { CapacityReason, ErrorCode } from "@cordial/protocol";
import { ConnectionClosedError, CordialError } from "@cordial/client";
import { describe, expect, it } from "vitest";
import { failure } from "../src/core/session.ts";
import { READ_FAILED, asSentence, codeText, errorText, reasonText } from "../src/shared/text.ts";

describe("error wording", () => {
  it("shows messages as sentences and inserts them as reasons without the period", () => {
    expect(codeText("busy")).toBe("The adapter is busy. Try again when the current operation finishes.");
    expect(errorText({ code: "no_capacity", reason: "enabled", outcomeUnknown: false })).toBe(
      "Every enabled-device place is in use. Turn off another device first.",
    );
    expect(reasonText(codeText("transport_error"))).toBe("The adapter couldn't exchange messages with this device");
    expect(asSentence("couldn't open the port")).toBe("Couldn't open the port.");
    expect(asSentence("Done.")).toBe("Done.");
  });

  it("words a failed read of saved data as a read, and a failed save as a save", () => {
    const storage = new CordialError("listProfiles", ErrorCode.STORAGE_FAILED, CapacityReason.UNKNOWN, false);
    expect(failure(storage, true)).toBe(READ_FAILED);
    expect(failure(storage)).toBe("The adapter couldn't save the change. Your saved data hasn't changed.");
    expect(failure(new ConnectionClosedError(), true)).toBe("The adapter disconnected before it answered.");
    expect(failure(new ConnectionClosedError())).toBe("The adapter disconnected before the change finished.");
  });
});
