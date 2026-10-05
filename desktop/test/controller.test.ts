import { ErrorCode, Transport } from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { AdapterManager } from "../src/core/manager.ts";
import { FakeAdapter, device, profile } from "../src/fake/adapter.ts";
import type { AppState } from "../src/shared/state.ts";
import { inactiveText, transportDisabledText } from "../src/shared/text.ts";
import { controller, openSession, until } from "./helpers.ts";

/** Every adapter is ready and `devices` devices are listed. */
const loaded = (s: AppState | null, devices: number) =>
  !!s && s.adapters.length > 0 && s.adapters.every((a) => a.readiness === "ready") && s.devices.length === devices && s.devices.every((d) => d.settings !== null);

const commands = (fake: FakeAdapter) => fake.received.map((r) => r.command.case);

describe("Controller", () => {
  it("closes a reconnect that finishes opening after shutdown", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const open = vi.fn(async () => fake.open());
    const manager = new AdapterManager({
      listPorts: async () => [{ path: "/a", serial: fake.id }],
      openTransport: open,
      changed: vi.fn(),
      log: vi.fn(),
    });
    await manager.rescan();
    await until(() => !!manager.connected.get(fake.id)?.session.listed);
    await manager.disconnect(fake.id);
    const opening = Promise.withResolvers<void>();
    const close = vi.spyOn(fake, "close");
    let started = false;
    open.mockImplementationOnce(async () => {
      started = true;
      await opening.promise;
      return fake.open();
    });
    const reconnect = manager.connect(fake.id);
    await until(() => started);
    await manager.stop();
    opening.resolve();
    expect(await reconnect).toBe("This adapter is no longer available.");
    expect(manager.connected.size).toBe(0);
    expect(close).toHaveBeenCalledOnce();
    expect(await manager.connect(fake.id)).toBe("This adapter is no longer available.");
  });

  it("combines adapters and hides a port that isn't one", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const b = new FakeAdapter({ adapterId: "BBBB0002", board: "xiao_esp32s3" });
    const { c, deps, state } = controller({ "/a": a, "/b": b });
    const broken = new FakeAdapter({ adapterId: "CCCC0003" });
    broken.write = async () => {
      throw new Error("write failed");
    };
    deps.ports["/c"] = broken;
    await c.manager.rescan();
    await until(() => loaded(state(), 8));
    const s = state()!;
    expect(s.adapters.map((x) => [x.name, x.connection])).toEqual([
      ["Pico 2 W", "connected"],
      ["XIAO ESP32-S3", "connected"],
    ]);
    const mouse = s.devices.find((d) => d.key === "AAAA0001/2")!;
    expect(mouse).toMatchObject({ name: "Example Mouse", kind: "mouse" });
    expect(mouse.warnings!.map((w) => w.code)).toEqual(["pointer_selector_unsupported"]);
    expect(mouse.battery).toEqual({ percent: 12, charging: false, percentFresh: true, chargingFresh: true });
    expect(mouse.device.hidpp).toEqual({ kind: 1, enabled: true, version: { major: 4, minor: 5 }, state: "active", error: null });
    // Only setting values the device accepts writes for are settings; readings are information.
    expect(mouse.settings!.map((x) => x.key)).toEqual(["pointer.sensor.0.dpi", "wheel.invert", "wheel.mode"]);
    expect(mouse.device.info.find((f) => f.key === "wheel.diameter")).toEqual({ key: "wheel.diameter", type: "integer", value: 50 });
    await c.stop();
  });

  it.each(["session", "port list"])("retains configuration for 15 seconds after disconnect through the %s", async (source) => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", profiles: [profile(1, "Work")] });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4) && state()!.adapters[0]!.profilePage?.profiles.length === 1);
    const before = c.state().adapters[0]!;
    vi.useFakeTimers();
    try {
      delete deps.ports["/a"];
      if (source === "session") a.unplug();
      else await c.manager.rescan();
      await vi.advanceTimersByTimeAsync(0);
      expect(c.state().adapters).toEqual([{ ...before, connection: "connecting", readiness: "waiting", attention: [] }]);
      expect(c.state().devices).toEqual([]);
      expect(await c.act({ type: "profile.delete", adapterId: a.id, profile: 1 })).toMatchObject({ ok: false });
      expect(a.profiles).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(14999);
      expect(c.state().adapters).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(1);
      expect(c.state().adapters).toEqual([]);
      await vi.advanceTimersByTimeAsync(100);
      expect(state()!.adapters).toEqual([]);
    } finally {
      await c.stop();
      vi.useRealTimers();
    }
  });

  it("reconnects by adapter identity on a new port and cancels the expiry", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, ports, state } = controller({ "/a": a });
    ports.list = async () => Object.keys(deps.ports).map((path) => ({ path, serial: null }));
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    vi.useFakeTimers();
    try {
      delete deps.ports["/a"];
      a.unplug();
      await vi.advanceTimersByTimeAsync(14000);
      expect(c.state().adapters[0]?.connection).toBe("connecting");
      deps.ports["/b"] = a;
      await c.manager.rescan();
      await vi.advanceTimersByTimeAsync(100);
      expect(loaded(c.state(), 4)).toBe(true);
      expect(c.manager.reconnecting.size).toBe(0);
      await vi.advanceTimersByTimeAsync(2000);
      expect(c.state().adapters).toHaveLength(1);
      expect(c.state().adapters[0]?.connection).toBe("connected");
      delete deps.ports["/b"];
      await c.manager.rescan();
      expect(c.manager.reconnecting.size).toBe(1);
      await c.stop();
      expect(c.manager.reconnecting.size).toBe(0);
      await vi.advanceTimersByTimeAsync(100);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      await c.stop();
      vi.useRealTimers();
    }
  });

  it("keeps a manually disconnected adapter listed without opening it", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, ports, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    expect(await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => state()!.adapters[0]?.connection === "disconnected");
    expect(state()!.devices).toEqual([]);
    const opened = ports.opened;
    await c.manager.rescan();
    expect(ports.opened).toBe(opened);
    expect(state()!.adapters[0]?.connection).toBe("disconnected");
    expect(await c.act({ type: "adapter.connect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => loaded(state(), 4));
    await c.stop();
  });

  it("identifies adapters by their status when ports have no USB serial number", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, ports, state } = controller({ "/a": a });
    ports.list = async () => Object.keys(deps.ports).map((path) => ({ path, serial: null }));
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    expect(await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => state()!.adapters[0]?.connection === "disconnected");
    const opened = ports.opened;
    await c.manager.rescan();
    expect(ports.opened).toBe(opened);
    delete deps.ports["/a"];
    await c.manager.rescan();
    expect(await c.act({ type: "adapter.connect", adapterId: "AAAA0001" })).toEqual({ ok: false, message: "The adapter isn't plugged in." });
    await until(() => state()!.adapters.length === 0);
    // Plugged in again on another port: identified by its status, shown, still disconnected.
    deps.ports["/b"] = a;
    await c.manager.rescan();
    await until(() => state()!.adapters[0]?.connection === "disconnected");
    expect(await c.act({ type: "adapter.connect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => loaded(state(), 4));
    await c.stop();
  });

  it("ignores bytes left from an earlier session", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    a.staleInput = new Uint8Array([0x41, 0x42, 0x00, 0x03, 0x12, 0x34]);
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.stop();
  });

  it("notifies once for low and once for critical battery, then resets", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await until(() => deps.lowBattery.mock.calls.length === 1);
    expect(deps.lowBattery.mock.calls[0]![0]).toMatchObject({ key: "AAAA0001/2", level: "low", percent: 12 });
    const battery = () => state()!.devices.find((d) => d.key === "AAAA0001/2")!.battery;
    a.changeInfo(2, { "battery.level": 11 });
    await until(() => battery()?.percent === 11);
    a.changeInfo(2, { "battery.level": 4 });
    await until(() => deps.lowBattery.mock.calls.length === 2);
    expect(deps.lowBattery.mock.calls[1]![0]).toMatchObject({ level: "critical" });
    // Unknown doesn't reset; charging does.
    a.changeInfo(2, { "battery.level": null, "battery.charging": null });
    await until(() => battery() === null);
    a.changeInfo(2, { "battery.level": 4, "battery.charging": false });
    await until(() => battery()?.percent === 4);
    a.changeInfo(2, { "battery.charging": true });
    await until(() => battery()?.charging === true);
    expect(deps.lowBattery).toHaveBeenCalledTimes(2);
    a.changeInfo(2, { "battery.charging": false });
    await until(() => deps.lowBattery.mock.calls.length === 3);
    await c.stop();
  });

  it("changes device preferences, follows events and reports adapter errors in words", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    const entry = (id: string) => state()!.devices.find((d) => d.key === `AAAA0001/${id}`)!;
    expect(await c.act({ type: "device.update", key: "AAAA0001/3", trusted: false })).toEqual({ ok: true });
    await until(() => entry("3").device.trusted === false);
    expect(a.received.at(-1)!.command).toMatchObject({ case: "setDevice", value: { device: 3, trusted: false } });
    expect(await c.act({ type: "device.connect", key: "AAAA0001/4" })).toEqual({ ok: false, message: expect.stringContaining("turned off") });
    expect(await c.act({ type: "device.update", key: "AAAA0001/3", hidpp: true })).toEqual({ ok: true });
    expect(a.received.at(-1)!.command.value).toMatchObject({ integrations: [{ kind: 1, enabled: true }] });
    await until(() => entry("3").device.hidpp?.state === "disconnected");
    expect(await c.act({ type: "device.connect", key: "AAAA0001/3" })).toEqual({ ok: true });
    await until(() => entry("3").device.state === "connected");
    a.failures.disconnectDevice = [ErrorCode.BUSY];
    expect(await c.act({ type: "device.disconnect", key: "AAAA0001/3" })).toEqual({ ok: false, message: expect.stringContaining("busy") });
    expect(await c.act({ type: "device.refresh", key: "AAAA0001/4" })).toEqual({ ok: false, message: expect.stringContaining("isn't connected") });
    expect(commands(a).filter((x) => x === "refreshDevice")).toEqual([]);
    expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", platform: "windows" })).toEqual({ ok: true });
    await until(() => state()!.adapters[0]!.status?.platform === "windows");
    expect(await c.act({ type: "device.unpair", key: "AAAA0001/4" })).toEqual({ ok: true });
    await until(() => !state()!.devices.some((d) => d.key === "AAAA0001/4"));
    await c.stop();
  });

  it("doesn't offer enabling a device beyond the adapter's limit", async () => {
    const a = new FakeAdapter({
      adapterId: "AAAA0001",
      enabled: { classic: true },
      maxEnabled: 1,
      devices: [device(1), device(2, { enabled: false }), device(3, { transport: "classic", enabled: false })],
    });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const sent = a.received.length;
    expect(await c.act({ type: "device.update", key: "AAAA0001/2", enabled: true })).toEqual({
      ok: false,
      message: "Every enabled-device place is in use; turn off another device first",
    });
    expect(a.received.length).toBe(sent);
    // Another transport has its own places.
    expect(await c.act({ type: "device.update", key: "AAAA0001/3", enabled: true })).toEqual({ ok: true });
    // A refusal the app couldn't predict is shown as it is.
    a.maxEnabled = 0;
    expect(await c.act({ type: "device.update", key: "AAAA0001/1", enabled: false })).toEqual({ ok: true });
    await until(() => state()!.devices.find((d) => d.key === "AAAA0001/1")!.device.enabled === false);
    expect(await c.act({ type: "device.update", key: "AAAA0001/1", enabled: true })).toEqual({
      ok: false,
      message: "Every enabled-device place is in use; turn off another device first",
    });
    expect(commands(a).at(-1)).toBe("setDevice");
    await c.stop();
  });

  it("keeps a restarted scan running when the earlier scan's end arrives first", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    expect(await c.act({ type: "scan.start", adapterId: "AAAA0001" })).toEqual({ ok: true });
    expect(c.state().scan).toMatchObject({ running: true, candidates: [] });
    await until(() => state()!.scan?.candidates.length === 4);
    expect(c.state().scan!.running).toBe(true);
    await c.act({ type: "scan.stop" });
    await c.stop();
  });

  it("matches each scan's response to its own start when one is stopped before its answer", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    const starts = () => commands(a).filter((x) => x === "startScan").length;
    const release = a.hold();
    const first = c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => starts() === 1);
    // The client sends one request at a time, so the stop and the second start wait behind the
    // first start until it is answered.
    const stop = c.act({ type: "scan.stop" });
    const second = c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await new Promise((r) => setTimeout(r, 20));
    release();
    await Promise.all([first, stop, second]);
    expect(starts()).toBe(2);
    expect(c.state().scan).toMatchObject({ running: true });
    await until(() => state()!.scan?.candidates.length === 4);
    expect(c.state().scan!.running).toBe(true);
    await c.act({ type: "scan.stop" });
    await c.stop();
  });

  it("searches, pairs with numeric comparison and follows the saved device until it connects", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    expect(a.received.find((r) => r.command.case === "startScan")!.command.value).toMatchObject({ transports: [1, 2], seconds: 30 });
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 1 });
    expect(commands(a).slice(-2)).toEqual(["stopScan", "startPairing"]);
    await until(() => !!state()!.pairing?.prompt);
    expect(state()!.pairing!.prompt).toEqual({ kind: "confirm", value: "042731" });
    expect(await c.act({ type: "pair.reply", accept: true })).toEqual({ ok: true });
    expect(a.received.at(-1)!.command).toMatchObject({ case: "acceptPrompt", value: { value: "" } });
    await until(() => state()!.pairing?.phase === "connected");
    expect(state()!.devices.some((d) => d.name === "Example Keys Mini")).toBe(true);
    expect(await c.act({ type: "pair.reply", accept: true })).toEqual({ ok: false, message: "No pairing prompt is waiting." });
    await c.act({ type: "pair.dismiss" });
    await c.act({ type: "scan.stop" });
    expect(c.state().pairing).toBeNull();
    await c.stop();
  });

  it("sends a typed passkey and reports a rejected pairing", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true }, pairingMethod: "enter" });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 2 });
    await until(() => state()!.pairing?.prompt?.kind === "enter");
    expect(state()!.pairing!.prompt).toEqual({ kind: "enter", code: "passkey" });
    await c.act({ type: "pair.reply", accept: false });
    await until(() => state()!.pairing?.phase === "failed");
    expect(state()!.pairing!.message).toMatch(/rejected/);
    await c.stop();
  });

  it("distinguishes a cancelled pairing from a failed one and refuses pairing with full storage", async () => {
    const fake = new FakeAdapter();
    const { c, state } = controller({ "/fake": fake });
    try {
      await c.manager.rescan();
      await until(() => loaded(state(), 4));
      await c.act({ type: "scan.start", adapterId: fake.id });
      await until(() => !!state()?.scan?.candidates.length);
      await c.act({ type: "pair.start", adapterId: fake.id, candidateId: 1 });
      await until(() => !!state()?.pairing?.prompt);
      await c.act({ type: "pair.cancel" });
      await until(() => state()?.pairing?.phase === "cancelled");
      await c.act({ type: "pair.dismiss" });
      await c.act({ type: "pair.start", adapterId: fake.id, candidateId: 2 });
      await until(() => !!state()?.pairing?.prompt);
      await c.act({ type: "pair.reply", accept: false });
      await until(() => state()?.pairing?.phase === "failed");
      await c.act({ type: "pair.dismiss" });
      fake.changeAdapter({ storageFull: true });
      await until(() => state()!.adapters[0]!.attention.includes("The adapter's storage is full. Remove an unused device or forget a saved setting to pair another device."));
      const sent = fake.received.length;
      expect(await c.act({ type: "pair.start", adapterId: fake.id, candidateId: 2 })).toMatchObject({ ok: false, message: expect.stringContaining("storage") });
      expect(fake.received.length).toBe(sent);
    } finally {
      await c.stop();
    }
  });

  it("ends discovery and pairing with their adapter's session", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: a.id });
    await until(() => !!state()!.scan?.candidates.length);
    await c.act({ type: "pair.start", adapterId: a.id, candidateId: 1 });
    await until(() => !!state()!.pairing?.prompt);
    delete deps.ports["/a"];
    a.unplug();
    await until(() => state()!.adapters[0]?.connection === "connecting");
    expect(state()!.scan).toBeNull();
    expect(state()!.pairing).toBeNull();
    await c.stop();
  });
});

it("renames the adapter, restores its default name and keeps it across reconnects", async () => {
  const a = new FakeAdapter({ adapterId: "AAAA0001" });
  const b = new FakeAdapter({ adapterId: "BBBB0002" });
  const { c, state } = controller({ "/a": a, "/b": b });
  await c.manager.rescan();
  await until(() => loaded(state(), 8));
  expect(state()!.adapters.map((x) => x.name)).toEqual(["Pico 2 W", "Pico 2 W"]);
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "  Desk \u{10400}  " })).toEqual({ ok: true });
  await until(() => state()!.adapters.some((x) => x.name === "Desk \u{10400}"));
  expect(a.name).toBe("Desk \u{10400}");
  expect(await c.act({ type: "adapter.settings", adapterId: a.id, platform: "mac" })).toEqual({ ok: true });
  expect(a.received.at(-1)!.command.value).toMatchObject({ platform: 2 });
  expect((a.received.at(-1)!.command.value as { name?: string }).name).toBeUndefined();
  a.failures.setAdapter = [ErrorCode.STORAGE_FAILED];
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "Failed" })).toMatchObject({ ok: false });
  expect(a.name).toBe("Desk \u{10400}");
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "é".repeat(33) })).toMatchObject({ ok: false });
  await c.act({ type: "adapter.disconnect", adapterId: a.id });
  await until(() => state()!.adapters.some((x) => x.id === a.id && x.connection === "disconnected"));
  expect(state()!.adapters.find((x) => x.id === a.id)?.name).toBe("Desk \u{10400}");
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "Offline" })).toMatchObject({ ok: false });
  await c.act({ type: "adapter.connect", adapterId: a.id });
  await until(() => state()!.adapters.some((x) => x.id === a.id && x.readiness === "ready"));
  expect(state()!.adapters.find((x) => x.id === a.id)).toMatchObject({ name: "Desk \u{10400}", status: { platform: "mac" } });
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: null })).toEqual({ ok: true });
  expect(a.received.at(-1)!.command.value).toMatchObject({ name: "" });
  await until(() => state()!.adapters.find((x) => x.id === a.id)?.name === "Pico 2 W");
  expect(a.platform).toBe("mac");
  await c.stop();
});

it("enables and disables each transport and explains a disabled transport's devices", async () => {
  const a = new FakeAdapter({ adapterId: "AAAA0001" });
  const { c, state } = controller({ "/a": a });
  await c.manager.rescan();
  await until(() => loaded(state(), 4));
  const entry = (id: string) => state()!.devices.find((d) => d.key === `AAAA0001/${id}`)!;
  const status = () => state()!.adapters[0]!.status!;
  const CLASSIC_DISABLED = transportDisabledText("classic");
  // The firmware starts with Classic disabled and BLE enabled.
  expect(status().transports).toEqual([
    { transport: "classic", maxEnabled: 7, enabled: false },
    { transport: "ble", maxEnabled: 7, enabled: true },
  ]);
  expect(entry("3").device.inactive).toBe("transport_disabled");
  expect(inactiveText(entry("3").device)).toBe("Bluetooth Classic is disabled. Enable it in the adapter settings.");
  expect(await c.act({ type: "device.connect", key: "AAAA0001/3" })).toEqual({ ok: false, message: CLASSIC_DISABLED });
  expect(commands(a)).not.toContain("connectDevice");
  await c.act({ type: "scan.start", adapterId: "AAAA0001" });
  expect(a.received.at(-1)!.command.value).toMatchObject({ transports: [Transport.BLE] });
  await until(() => state()!.scan?.candidates.length === 3);
  await c.act({ type: "scan.stop" });

  expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", transports: { classic: true } })).toEqual({ ok: true });
  expect(a.received.at(-1)!.command.value).toMatchObject({ transports: [{ transport: Transport.CLASSIC, enabled: true }] });
  expect((a.received.at(-1)!.command.value as { name?: string; platform?: number }).name).toBeUndefined();
  await until(() => status().transports[0]!.enabled && entry("3").device.inactive === null);
  expect(await c.act({ type: "device.connect", key: "AAAA0001/3" })).toEqual({ ok: true });
  await until(() => entry("3").device.state === "connected");

  // Disabling a transport closes its links and ends a pairing over it; a candidate found
  // earlier isn't paired again.
  await c.act({ type: "scan.start", adapterId: "AAAA0001" });
  await until(() => state()!.scan?.candidates.length === 4);
  expect(await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 3 })).toEqual({ ok: true });
  await until(() => !!state()!.pairing?.prompt);
  expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", transports: { classic: false } })).toEqual({ ok: true });
  await until(() => !status().transports[0]!.enabled && entry("3").device.inactive === "transport_disabled" && entry("3").device.state === "disconnected");
  await until(() => state()!.pairing?.phase === "failed");
  expect(state()!.pairing!.message).toBe(CLASSIC_DISABLED);
  await c.act({ type: "pair.dismiss" });
  const pairings = commands(a).filter((x) => x === "startPairing").length;
  expect(await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 3 })).toEqual({ ok: false, message: CLASSIC_DISABLED });
  expect(commands(a).filter((x) => x === "startPairing").length).toBe(pairings);

  // With every transport disabled nothing is scanned, and BLE devices say so.
  expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", transports: { ble: false } })).toEqual({ ok: true });
  await until(() => entry("1").device.inactive === "transport_disabled");
  const BLE_DISABLED = "Bluetooth LE is disabled. Enable it in the adapter settings.";
  expect(inactiveText(entry("1").device)).toBe(BLE_DISABLED);
  const scans = commands(a).filter((x) => x === "startScan").length;
  expect(await c.act({ type: "scan.start", adapterId: "AAAA0001" })).toEqual({ ok: false, message: BLE_DISABLED });
  expect(commands(a).filter((x) => x === "startScan").length).toBe(scans);

  // Saving the setting can fail like the other adapter settings.
  a.failures.setAdapter = [ErrorCode.STORAGE_FAILED];
  expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", transports: { ble: true } })).toEqual({
    ok: false,
    message: "The adapter couldn't save the change. Your saved data hasn't changed",
  });
  await c.stop();
});

it("names a disabled transport when the adapter refuses work on it after it was disabled elsewhere", async () => {
  const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
  const { c, state } = controller({ "/a": a });
  await c.manager.rescan();
  await until(() => loaded(state(), 4));
  // The adapter reports Classic disabled before the device's own update arrives, so the app
  // still sends the request.
  a.enabled.classic = false;
  a.changeAdapter({});
  await until(() => !state()!.adapters[0]!.status!.transports[0]!.enabled);
  expect(state()!.devices.find((d) => d.key === "AAAA0001/3")!.device.inactive).toBeNull();
  expect(await c.act({ type: "device.connect", key: "AAAA0001/3" })).toEqual({ ok: false, message: transportDisabledText("classic") });
  expect(commands(a).at(-1)).toBe("connectDevice");
  await c.stop();
});

it("names a disabled transport when the adapter refuses a scan because the transport was just disabled", async () => {
  const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true, ble: false } });
  const { c, state } = controller({ "/a": a });
  await c.manager.rescan();
  await until(() => loaded(state(), 4));
  // Classic is disabled elsewhere; its adapter event arrives just before the scan's answer.
  const release = a.hold();
  a.enabled.classic = false;
  a.changeAdapter({});
  const started = c.act({ type: "scan.start", adapterId: "AAAA0001" });
  await until(() => commands(a).includes("startScan"));
  expect(a.received.at(-1)!.command.value).toMatchObject({ transports: [Transport.CLASSIC] });
  release();
  expect(await started).toEqual({ ok: false, message: transportDisabledText("classic") });
  await c.stop();
});

it("refuses changing a transport the adapter doesn't support", async () => {
  const a = new FakeAdapter({ adapterId: "AAAA0001", board: "xiao_esp32s3", transports: ["ble"] });
  const { c, state } = controller({ "/a": a });
  await c.manager.rescan();
  await until(() => loaded(state(), 4));
  expect(state()!.adapters[0]!.status!.transports).toEqual([{ transport: "ble", maxEnabled: 7, enabled: true }]);
  const sent = a.received.length;
  expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", transports: { classic: true } })).toEqual({
    ok: false,
    message: "The adapter or device doesn't support this action",
  });
  expect(a.received.length).toBe(sent);
  await c.stop();
});

it("treats a transport whose enabled flag is missing as enabled", async () => {
  const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
  const status = a.status.bind(a);
  a.status = () => {
    const s = status();
    for (const t of s.transports) t.enabled = undefined;
    return s;
  };
  const { c, state } = controller({ "/a": a });
  await c.manager.rescan();
  await until(() => loaded(state(), 4));
  expect(state()!.adapters[0]!.status!.transports).toEqual([
    { transport: "classic", maxEnabled: 7, enabled: true },
    { transport: "ble", maxEnabled: 7, enabled: true },
  ]);
  await c.act({ type: "scan.start", adapterId: "AAAA0001" });
  expect(a.received.at(-1)!.command.value).toMatchObject({ transports: [Transport.CLASSIC, Transport.BLE] });
  await c.act({ type: "scan.stop" });
  await c.stop();
});

it("changes nothing when a transport update names a transport the adapter doesn't support", async () => {
  const { fake, session } = await openSession({ transports: ["ble"] });
  await expect(
    session.connection.setAdapter({ transports: [{ transport: Transport.BLE, enabled: false }, { transport: Transport.CLASSIC, enabled: true }] }),
  ).rejects.toMatchObject({ code: ErrorCode.UNSUPPORTED });
  await expect(session.connection.setAdapter({ transports: [{ transport: Transport.UNSPECIFIED, enabled: true }] })).rejects.toMatchObject({
    code: ErrorCode.BAD_ARGS,
  });
  expect(fake.enabled.ble).toBe(true);
  await session.close();
});
