import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BatteryAlerts, batteryLevel } from "../src/core/battery.ts";
import { trayModel } from "../src/main/tray-model.ts";
import { IMAGE_INTERVAL, trayIcon } from "../src/main/tray.ts";
import { DEFAULT_PREFERENCES, type AdapterEntry, type AppState, type DeviceEntry } from "../src/shared/state.ts";
import { record } from "./helpers.ts";

const adapter = (id: string, patch: Partial<AdapterEntry> = {}): AdapterEntry => ({
  id,
  name: `Adapter ${id}`,
  connection: "connected",
  connectError: null,
  readiness: "ready",
  status: null,
  refused: null,
  listing: false,
  listError: null,
  attention: [],
  profilePage: null,
  pickerPage: null,
  profileNames: {},
  profilesAvailable: true,
  unreadableDevices: [],
  migration: null,
  ...patch,
});

const entry = (adapterId: string, id: string, patch: Partial<DeviceEntry> = {}, state = "disconnected"): DeviceEntry => ({
  key: `${adapterId}/${id}`,
  adapterId,
  device: record(Number(id), { state: state as never }),
  name: id,
  kind: "keyboard",
  battery: null,
  pending: [],
  settings: null,
  settingsAvailable: false,
  settingsError: null,
  readError: null,
  available: true,
  settingsSave: null,
  ...patch,
});

const app = (adapters: AdapterEntry[], devices: DeviceEntry[], alwaysShowTray = false): AppState => ({
  adapters,
  devices,
  scan: null,
  pairing: null,
  preferences: { ...DEFAULT_PREFERENCES, alwaysShowTray },
  hostPlatform: "linux",
  revision: 1,
});

describe("trayModel", () => {
  it("is hidden without a connected adapter unless always shown", () => {
    expect(trayModel(app([], [])).visible).toBe(false);
    expect(trayModel(app([adapter("A", { connection: "disconnected" })], [])).visible).toBe(false);
    expect(trayModel(app([], [], true)).visible).toBe(true);
  });

  it("fills when a device is connected and badges attention before low battery", () => {
    const idle = trayModel(app([adapter("A")], [entry("A", "1")]));
    expect([idle.visible, idle.base, idle.badge]).toEqual([true, "idle", "none"]);
    const lowEntry = entry("A", "1", { battery: { percent: 20, charging: false, percentFresh: true, chargingFresh: true } }, "connected");
    const attention = trayModel(app([adapter("A", { attention: ["1 device needs pairing again"] })], [lowEntry]));
    expect([attention.base, attention.badge]).toEqual(["connected", "attention"]);
    const low = trayModel(app([adapter("A")], [lowEntry]));
    expect([low.base, low.badge]).toEqual(["connected", "low"]);
    expect(low.groups[0]!.devices[0]!.label).toBe("1 — Connected · 20% (low)");
    expect(low.tooltip).toContain("1 device connected");
  });

  it("groups devices by adapter only with several adapters", () => {
    const one = trayModel(app([adapter("A")], [entry("A", "1")]));
    expect(one.groups.map((g) => g.title)).toEqual([null]);
    expect(one.groups[0]!.devices[0]!.label).toBe("1 — Disconnected");
    const two = trayModel(app([adapter("A"), adapter("B")], [entry("A", "1"), entry("B", "1")]));
    expect(two.groups.map((g) => [g.title, g.devices.length])).toEqual([
      ["Adapter A", 1],
      ["Adapter B", 1],
    ]);
  });

  it("names what is missing when there are no devices to list", () => {
    expect(trayModel(app([adapter("A")], [entry("A", "1")])).empty).toBeNull();
    expect(trayModel(app([adapter("A")], [])).empty).toBe("No Saved Devices");
    expect(trayModel(app([], [])).empty).toBe("No Adapter Connected");
    expect(trayModel(app([adapter("A", { connection: "connecting" }), adapter("B", { connection: "disconnected" })], [entry("A", "1")])).empty).toBe("No Adapter Connected");
  });

  it("offers Connect only through a ready adapter", () => {
    const ready = trayModel(app([adapter("A")], [entry("A", "1")]));
    expect(ready.groups[0]!.devices[0]!.canConnect).toBe(true);
    const waiting = trayModel(app([adapter("A", { readiness: "waiting" })], [entry("A", "1")]));
    expect(waiting.groups[0]!.devices[0]!.canConnect).toBe(false);
  });
});

describe("trayIcon", () => {
  beforeEach(() => vi.useFakeTimers({ now: 0 }));
  afterEach(() => vi.useRealTimers());

  /** A tray icon whose trays record what reaches them, with the time. */
  const setup = () => {
    const events: string[] = [];
    const record = (what: string) => events.push(`${Date.now()} ${what}`);
    const icon = trayIcon((image) => {
      record(`create ${image}`);
      let destroyed = false;
      return {
        setImage: (next: string) => record(destroyed ? `setImage on destroyed tray` : `setImage ${next}`),
        setToolTip: (text: string) => record(destroyed ? `setToolTip on destroyed tray` : `tooltip ${text}`),
        destroy: () => {
          destroyed = true;
          record("destroy");
        },
      };
    });
    const update = (image: string, tooltip = "T") => icon.update(image, tooltip, (t) => t.setToolTip(tooltip));
    const take = () => events.splice(0);
    return { icon, update, take };
  };

  it("creates the tray at once and leaves it alone for unchanged requests", () => {
    const { update, take } = setup();
    update("a");
    expect(take()).toEqual(["0 create a", "0 tooltip T"]);
    update("a");
    vi.advanceTimersByTime(5000);
    update("a");
    expect(take()).toEqual([]);
  });

  it("changes the menu and tooltip at once while the image waits", () => {
    const { update, take } = setup();
    update("a");
    take();
    vi.advanceTimersByTime(100);
    update("b", "U");
    expect(take()).toEqual(["100 tooltip U"]);
    vi.advanceTimersByTime(IMAGE_INTERVAL);
    expect(take()).toEqual(["1000 setImage b"]);
  });

  it("applies the first change after a cooldown at once and the latest of later ones when it ends", () => {
    const { update, take } = setup();
    update("a");
    vi.advanceTimersByTime(1500);
    take();
    update("b");
    expect(take()).toEqual(["1500 setImage b"]);
    vi.advanceTimersByTime(200);
    update("c");
    vi.advanceTimersByTime(200);
    update("d");
    expect(take()).toEqual([]);
    vi.advanceTimersByTime(600);
    expect(take()).toEqual(["2500 setImage d"]);
    // The trailing change starts a fresh cooldown.
    update("e");
    vi.advanceTimersByTime(999);
    expect(take()).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(take()).toEqual(["3500 setImage e"]);
  });

  it("drops a waiting change when requests return to the shown image", () => {
    const { update, take } = setup();
    update("a");
    take();
    vi.advanceTimersByTime(300);
    update("b");
    vi.advanceTimersByTime(300);
    update("a");
    vi.advanceTimersByTime(5000);
    expect(take()).toEqual([]);
  });

  it("applies a change made after a return to the shown image when the original cooldown ends", () => {
    const { update, take } = setup();
    update("a");
    take();
    vi.advanceTimersByTime(300);
    update("b");
    update("a");
    vi.advanceTimersByTime(300);
    update("c");
    vi.advanceTimersByTime(399);
    expect(take()).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(take()).toEqual(["1000 setImage c"]);
  });

  it("keeps changing the image at most once a second under continuous requests", () => {
    const { update, take } = setup();
    update("i0");
    take();
    for (let i = 1; i <= 50; i++) {
      vi.advanceTimersByTime(100);
      update(`i${i}`);
    }
    vi.advanceTimersByTime(IMAGE_INTERVAL);
    // Each cooldown ends just before the request made at the same time.
    expect(take()).toEqual([
      "1000 setImage i9",
      "2000 setImage i19",
      "3000 setImage i29",
      "4000 setImage i39",
      "5000 setImage i49",
      "6000 setImage i50",
    ]);
  });

  it("drops a waiting change when hidden and creates the next tray with the latest image", () => {
    const { icon, update, take } = setup();
    update("a");
    vi.advanceTimersByTime(100);
    update("b");
    icon.hide();
    vi.advanceTimersByTime(5000);
    expect(take()).toEqual(["0 create a", "0 tooltip T", "100 destroy"]);
    icon.hide();
    update("c");
    expect(take()).toEqual(["5100 create c", "5100 tooltip T"]);
    // Creation shows an image, so the next change waits.
    update("d");
    vi.advanceTimersByTime(999);
    expect(take()).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(take()).toEqual(["6100 setImage d"]);
  });

  it("follows the theme's image variant only when it changes", () => {
    const { update, take } = setup();
    update("tray-idle-none-dark.png");
    vi.advanceTimersByTime(2000);
    take();
    // A theme update that keeps the variant.
    update("tray-idle-none-dark.png");
    expect(take()).toEqual([]);
    update("tray-idle-none-light.png");
    vi.advanceTimersByTime(100);
    update("tray-connected-none-light.png");
    vi.advanceTimersByTime(900);
    expect(take()).toEqual(["2000 setImage tray-idle-none-light.png", "3000 setImage tray-connected-none-light.png"]);
  });
});

describe("battery", () => {
  it("treats unknown as undecided and charging as not low", () => {
    expect(batteryLevel(null, 20)).toBeNull();
    expect(batteryLevel({ percent: null, charging: false, percentFresh: true, chargingFresh: true }, 20)).toBeNull();
    expect(batteryLevel({ percent: 3, charging: true, percentFresh: true, chargingFresh: true }, 20)).toBe("ok");
    expect(batteryLevel({ percent: 20, charging: null, percentFresh: true, chargingFresh: false }, 20)).toBe("low");
    expect(batteryLevel({ percent: 5, charging: false, percentFresh: true, chargingFresh: true }, 20)).toBe("critical");
  });

  it("alerts on entering low and critical once", () => {
    const alerts = new BatteryAlerts();
    const d = (percent: number | null, charging: boolean | null = false) => [
      entry("A", "1", { battery: percent == null && charging == null ? null : { percent, charging, percentFresh: percent !== null, chargingFresh: charging !== null } }),
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
