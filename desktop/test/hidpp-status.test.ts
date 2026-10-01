import { describe, expect, it } from "vitest";
import { device } from "../src/fake/adapter.ts";
import { responseProblem } from "../src/protocol/validate.ts";
import { hidppProtocolText } from "../src/shared/text.ts";
import { openSession, until } from "./helpers.ts";

const response = (d: unknown) => ({ v: 1, type: "response", id: 1, ok: true, done: true, result: { revision: 1, device: d } });

describe("HID++ protocol evidence", () => {
  it("starts each simulated connection with fresh evidence and preserves preference changes", async () => {
    const { fake, session } = await openSession();
    await until(() => session.view.valid);
    expect(fake.find("d_1").device.hidpp_protocol?.state).toBe("detected");
    await session.request("device.hidpp.set", { device_id: "d_1", enabled: false });
    expect(fake.find("d_1").device.hidpp_protocol?.state).toBe("detected");
    await session.request("device.disconnect", { device_id: "d_1" });
    expect(fake.find("d_1").device.hidpp_protocol?.state).toBe("unknown");
    const connecting = session.request("device.connect", { device_id: "d_1" });
    await until(() => fake.find("d_1").device.state === "connecting");
    expect(fake.find("d_1").device.hidpp_protocol?.state).toBe("unknown");
    await connecting;
    expect(fake.find("d_1").device.hidpp_protocol).toEqual({ state: "detected", major: 4, minor: 5 });
    await session.close();
  });

  it("accepts detected protocol with missing translation features", () => {
    const d = device("d_1", {
      state: "connected",
      hidpp_protocol: { state: "detected", major: 4, minor: 2 },
      normalization_state: "unsupported",
      normalization_error: "hidpp_controls_unavailable",
      settings_state: "ready",
    });
    expect(responseProblem("device.get", response(d))).toBeNull();
    expect(hidppProtocolText(d.hidpp_protocol)).toBe("4.2");
    expect(hidppProtocolText({ state: "error", code: "hidpp_timeout" })).toBe("Failed: No response");
  });

  it("treats absent evidence as unknown and rejects invalid results", () => {
    const d = device("d_1");
    delete d.hidpp_protocol;
    expect(responseProblem("device.get", response(d))).toBeNull();
    expect(hidppProtocolText(d.hidpp_protocol)).toBe("Unknown");
    for (const protocol of [
      { state: "detected", major: 0, minor: 0 },
      { state: "detected", major: 4, minor: 256 },
      { state: "error", code: "hidpp_controls_unavailable" },
      { state: "unknown", major: 4 },
    ]) {
      expect(responseProblem("device.get", response({ ...d, hidpp_protocol: protocol }))).not.toBeNull();
    }
  });
});
