import { describe, expect, it } from "vitest";
import { device } from "../src/fake/adapter.ts";
import { BatteryAlerts, batteryLevel } from "../src/core/battery.ts";
import { trayModel } from "../src/main/tray-model.ts";
import { DEFAULT_PREFERENCES, type AdapterEntry, type AppState, type DeviceEntry } from "../src/shared/state.ts";

const adapter = (id: string, patch: Partial<AdapterEntry> = {}): AdapterEntry => ({
  id,
  name: `Adapter ${id}`,
  connection: "connected",
  connectError: null,
  readiness: "ready",
  status: null,
  capabilities: ["ble"],
  platform: "linux",
  attention: [],
  ...patch,
});

const entry = (adapterId: string, id: string, patch: Partial<DeviceEntry> = {}, state = "disconnected"): DeviceEntry => ({
  key: `${adapterId}/${id}`,
  adapterId,
  device: device(id, { state: state as never }),
  name: id,
  kind: "keyboard",
  battery: null,
  info: [],
  infoError: null,
  settings: null,
  ...patch,
});

const app = (adapters: AdapterEntry[], devices: DeviceEntry[], alwaysShowTray = false): AppState => ({
  adapters,
  devices,
  scan: null,
  pairing: null,
  preferences: { ...DEFAULT_PREFERENCES, alwaysShowTray },
  hostPlatform: "linux",
});

describe("trayModel", () => {
  it("is hidden without a connected adapter unless always shown", () => {
    expect(trayModel(app([], [])).visible).toBe(false);
    expect(trayModel(app([adapter("A", { connection: "disconnected" })], [])).visible).toBe(false);
    expect(trayModel(app([], [], true)).visible).toBe(true);
  });

  it("fills when a device is connected and badges low battery first", () => {
    const idle = trayModel(app([adapter("A")], [entry("A", "d_1")]));
    expect([idle.visible, idle.base, idle.badge]).toEqual([true, "idle", "none"]);
    const low = trayModel(
      app(
        [adapter("A", { attention: ["1 device needs pairing again"] })],
        [entry("A", "d_1", { battery: { percent: 20, charging: false } }, "connected")],
      ),
    );
    expect([low.base, low.badge]).toEqual(["connected", "low"]);
    expect(low.groups[0]!.devices[0]!.label).toBe("d_1 — Connected · 20% (low)");
    expect(low.tooltip).toContain("1 device connected");
  });

  it("groups devices by adapter only with several adapters", () => {
    const one = trayModel(app([adapter("A")], [entry("A", "d_1")]));
    expect(one.groups.map((g) => g.title)).toEqual([null]);
    const two = trayModel(app([adapter("A"), adapter("B")], [entry("A", "d_1"), entry("B", "d_1")]));
    expect(two.groups.map((g) => [g.title, g.devices.length])).toEqual([
      ["Adapter A", 1],
      ["Adapter B", 1],
    ]);
  });
});

describe("battery", () => {
  it("treats unknown as undecided and charging as not low", () => {
    expect(batteryLevel(null, 20)).toBeNull();
    expect(batteryLevel({ percent: null, charging: false }, 20)).toBeNull();
    expect(batteryLevel({ percent: 3, charging: true }, 20)).toBe("ok");
    expect(batteryLevel({ percent: 20, charging: null }, 20)).toBe("low");
    expect(batteryLevel({ percent: 5, charging: false }, 20)).toBe("critical");
  });

  it("alerts on entering low and critical once", () => {
    const alerts = new BatteryAlerts();
    const d = (percent: number | null, charging: boolean | null = false) => [
      entry("A", "d_1", { battery: percent == null && charging == null ? null : { percent, charging } }),
    ];
    expect(alerts.update(d(50), 20)).toEqual([]);
    expect(alerts.update(d(20), 20).map((a) => a.level)).toEqual(["low"]);
    expect(alerts.update(d(19), 20)).toEqual([]);
    expect(alerts.update(d(null, null), 20)).toEqual([]);
    expect(alerts.update(d(18), 20)).toEqual([]);
    expect(alerts.update(d(5), 20).map((a) => a.level)).toEqual(["critical"]);
    expect(alerts.update(d(10), 20)).toEqual([]);
    expect(alerts.update(d(4), 20)).toEqual([]);
    expect(alerts.update(d(21), 20)).toEqual([]);
    expect(alerts.update(d(15), 20).map((a) => a.level)).toEqual(["low"]);
  });
});

describe("clean", () => {
  it("drops control and bidirectional format characters", async () => {
    const { clean } = await import("../src/shared/text.ts");
    expect(clean("A\u202eB\u2066C\u0007D\u200bE\u2028F ")).toBe("ABC DE F");
  });
});
