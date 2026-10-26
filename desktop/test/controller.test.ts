import { ErrorCode, ErrorOutcome, ErrorReason, Transport } from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { AdapterManager } from "../src/core/manager.ts";
import { AdapterSession } from "../src/core/session.ts";
import { FakeAdapter, device, profile } from "../src/fake/adapter.ts";
import type { AppState } from "../src/shared/state.ts";
import { READ_FAILED, adapterStatus, codeText, connected, deviceStatus, inactiveText, transportDisabledText } from "../src/shared/text.ts";
import { controller, openSession, until } from "./helpers.ts";

/** Every adapter is ready and `devices` devices are listed. */
const loaded = (s: AppState | null, devices: number) =>
  !!s && s.adapters.length > 0 && s.adapters.every((a) => a.readiness === "ready") && s.devices.length === devices && s.devices.every((d) => d.settings !== null);

const commands = (fake: FakeAdapter) => fake.received.map((r) => r.command.case);

describe("Controller", () => {
  it("keeps a fresh adapter connected while its first device list is pending", async () => {
    const fake = new FakeAdapter();
    const { c } = controller({ "/a": fake });
    let release = () => {};
    const write = fake.write.bind(fake);
    const spy = vi.spyOn(fake, "write").mockImplementation(async (bytes) => {
      await write(bytes);
      if (fake.received.at(-1)?.command.case === "getStatus") release = fake.hold();
    });
    try {
      await c.manager.rescan();
      await until(() => fake.received.some((r) => r.command.case === "listDevices"));
      expect(c.state().adapters[0]).toMatchObject({ connection: "connected", listing: true, listError: null });
      expect(adapterStatus(c.state().adapters[0]!)).toEqual({ text: "Starting…", warn: false });
      expect(c.manager.connected.get(fake.id)!.session.available).toBe(false);
      release();
      await until(() => loaded(c.state(), 4));
    } finally {
      spy.mockRestore();
      release();
      await c.stop();
    }
  });

  it("retries a failed first device list on the live adapter", async () => {
    const fake = new FakeAdapter();
    fake.failures.listDevices = [ErrorCode.STORAGE_FAILED];
    const { c, deps } = controller({ "/a": fake });
    let release = () => {};
    try {
      await c.manager.rescan();
      await until(() => deps.log.mock.calls.some(([message]) => message.includes("listing devices failed")) && !c.state().adapters[0]!.listing);
      const entry = c.manager.connected.get(fake.id)!;
      expect(entry.recovery).toBeNull();
      expect(entry.session.available).toBe(false);
      expect(c.state().adapters[0]).toMatchObject({ connection: "connected", listing: false, listError: READ_FAILED });
      expect(adapterStatus(c.state().adapters[0]!)).toEqual({ text: "Needs Attention", warn: true });
      // While Retry lists again, the adapter is starting and the failure is gone.
      release = fake.hold();
      expect(await c.act({ type: "devices.reload", adapterId: fake.id })).toEqual({ ok: true });
      await until(() => fake.received.filter((r) => r.command.case === "listDevices").length === 2);
      expect(c.state().adapters[0]).toMatchObject({ connection: "connected", listing: true, listError: null });
      expect(adapterStatus(c.state().adapters[0]!)).toEqual({ text: "Starting…", warn: false });
      release();
      await until(() => loaded(c.state(), 4));
      expect(c.state().adapters[0]).toMatchObject({ listing: false, listError: null });
      expect(adapterStatus(c.state().adapters[0]!)).toEqual({ text: "Ready", warn: false });
      expect(c.manager.connected.get(fake.id)).toBe(entry);
      expect(fake.received.filter((r) => r.command.case === "listDevices")).toHaveLength(2);
    } finally {
      release();
      await c.stop();
    }
  });

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

  it("doesn't keep a session that ended while it was opening", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const log = vi.fn();
    const manager = new AdapterManager({
      listPorts: async () => [{ path: "/a", serial: fake.id }],
      openTransport: async () => fake.open(),
      changed: vi.fn(),
      log,
    });
    const open = AdapterSession.open.bind(AdapterSession);
    vi.spyOn(AdapterSession, "open").mockImplementationOnce(async (stream, hooks) => {
      const session = await open(stream, hooks);
      fake.unplug();
      return session;
    });
    await manager.rescan();
    expect(manager.connected.size).toBe(0);
    await until(() => log.mock.calls.some(([message]) => message === "/a: session ended while opening"));
    await manager.stop();
  });

  it("combines adapters and hides a port that isn't one", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const b = new FakeAdapter({ adapterId: "BBBB0002", board: "esp32s3" });
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
      ["ESP32-S3", "connected"],
      ["Pico 2 W", "connected"],
    ]);
    const mouse = s.devices.find((d) => d.key === "AAAA0001/2")!;
    expect(mouse).toMatchObject({ name: "Example Mouse", kind: "mouse" });
    expect(mouse.battery).toEqual({ percent: 12, charging: false, percentFresh: true, chargingFresh: true });
    expect(mouse.device.hidpp).toEqual({ kind: 1, enabled: true, version: { major: 4, minor: 5 }, state: "active", error: null });
    // Only setting values the device accepts writes for are settings; readings are information.
    expect(mouse.settings!.map((x) => `${x.group}.${x.field}`)).toEqual(["pointer_sensor.dpi", "wheel.mode", "wheel.invert"]);
    expect(mouse.device.information.wheel).toMatchObject({ diameter: 50, mode: null });
    await c.stop();
  });

  it("shows a record the adapter couldn't read as last known, and one it never read as a placeholder", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    a.unreadableDevices.add(4);
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const error = { code: "storage_failed", reason: "storage", outcome: "no_effect", related: [] };
    expect(state()!.adapters[0]!.unreadableDevices).toEqual([{ id: 4, error }]);
    // A connected device whose record can't be read again keeps its last record, not its state.
    a.unreadableDevices.add(2);
    a.unreadableDevices.delete(4);
    expect(await c.act({ type: "devices.reload", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => !!state()!.devices.find((d) => d.key === "AAAA0001/2")?.readError && state()!.devices.length === 4);
    const mouse = state()!.devices.find((d) => d.key === "AAAA0001/2")!;
    expect(mouse).toMatchObject({ readError: error, device: { state: "connected" } });
    expect(deviceStatus(mouse)).toBe("Couldn't Read");
    expect(connected(mouse)).toBe(false);
    expect(mouse.battery).toMatchObject({ percent: 12, percentFresh: false });
    expect(state()!.adapters[0]!.unreadableDevices).toEqual([]);
    // Refresh goes to the adapter rather than being refused from the last record.
    expect(await c.act({ type: "device.refresh", key: "AAAA0001/2" })).toEqual({ ok: true });
    // The device's next record is current again.
    a.unreadableDevices.clear();
    a.changeDevice(2, { name: "Renamed" });
    await until(() => state()!.devices.find((d) => d.key === "AAAA0001/2")?.readError === null);
    expect(deviceStatus(state()!.devices.find((d) => d.key === "AAAA0001/2")!)).toBe("Connected");
    await c.stop();
  });

  it("reports a radio or storage fault in place of starting", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", ready: false });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => state()?.adapters[0]?.connection === "connected");
    expect(adapterStatus(state()!.adapters[0]!)).toEqual({ text: "Starting…", warn: false });
    a.faults = [{ code: ErrorCode.NOT_READY, reason: ErrorReason.RADIO }];
    a.changeAdapter({});
    await until(() => state()!.adapters[0]!.status!.errors.length === 1);
    expect(adapterStatus(state()!.adapters[0]!)).toEqual({ text: "Needs Attention", warn: true });
    expect(state()!.adapters[0]!.attention).toEqual([]);
    await c.stop();
  });

  it.each(["session", "port list"])("retains configuration for 15 seconds after disconnect through the %s", async (source) => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", profiles: [profile(1, "Work")] });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4) && state()!.adapters[0]!.profilePage?.profiles.length === 1);
    const before = c.state().adapters[0]!;
    const devices = c.state().devices;
    vi.useFakeTimers();
    try {
      delete deps.ports["/a"];
      if (source === "session") a.unplug();
      else await c.manager.rescan();
      await vi.advanceTimersByTimeAsync(0);
      expect(c.state().adapters).toEqual([{ ...before, connection: "reconnecting", readiness: "waiting", profilesAvailable: false }]);
      expect(c.state().devices).toEqual(devices.map((d) => ({ ...d, available: false, settingsAvailable: false, battery: d.battery && { ...d.battery, percentFresh: false, chargingFresh: false } })));
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

  it("waits for a returning adapter's devices only while it is listing them", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    expect(c.state().adapters[0]!.listing).toBe(false);
    delete deps.ports["/a"];
    a.unplug();
    await until(() => c.state().adapters[0]?.connection === "reconnecting");
    expect(c.state().adapters[0]!.listing).toBe(false);
    const before = c.state().devices;
    a.failures.listDevices = [ErrorCode.STORAGE_FAILED];
    deps.ports["/b"] = a;
    await c.manager.rescan();
    await until(() => a.received.some((r) => r.command.case === "listDevices"));
    expect(c.state().adapters[0]!.connection).toBe("reconnecting");
    expect(c.state().devices).toEqual(before);
    // A failed listing during recovery leaves the adapter reconnecting rather than reporting it.
    await until(() => deps.log.mock.calls.some(([message]) => message.includes("listing devices failed")));
    expect(c.state().adapters[0]).toMatchObject({ connection: "reconnecting", listError: null });
    await until(() => c.state().adapters[0]?.connection === "connected");
    expect(c.state().devices).toHaveLength(4);
    await c.stop();
  });

  it("reconnects by adapter identity on a new port and cancels the expiry", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    vi.useFakeTimers();
    try {
      delete deps.ports["/a"];
      a.unplug();
      await vi.advanceTimersByTimeAsync(14000);
      expect(c.state().adapters[0]?.connection).toBe("reconnecting");
      deps.ports["/b"] = a;
      await c.manager.rescan();
      await vi.advanceTimersByTimeAsync(100);
      expect(loaded(c.state(), 4)).toBe(true);
      expect([...c.manager.connected.values()].some((c) => c.recovery)).toBe(false);
      await vi.advanceTimersByTimeAsync(2000);
      expect(c.state().adapters).toHaveLength(1);
      expect(c.state().adapters[0]?.connection).toBe("connected");
      delete deps.ports["/b"];
      await c.manager.rescan();
      expect([...c.manager.connected.values()].some((c) => c.recovery)).toBe(true);
      await c.stop();
      expect([...c.manager.connected.values()].some((c) => c.recovery)).toBe(false);
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

  it("ignores empty frames before the hello and sends nothing after any other frame", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    a.staleInput = new Uint8Array([0x41, 0x42, 0x00, 0x03, 0x12, 0x34]);
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => a.hellos === 1);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(a.received).toEqual([]);
    expect(state()?.adapters ?? []).toEqual([]);
    a.staleInput = new Uint8Array([0x00, 0x00]);
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
    a.changeInfo(2, { battery: { level: 11 } });
    await until(() => battery()?.percent === 11);
    a.changeInfo(2, { battery: { level: 4 } });
    await until(() => deps.lowBattery.mock.calls.length === 2);
    expect(deps.lowBattery.mock.calls[1]![0]).toMatchObject({ level: "critical" });
    // Unknown doesn't reset; charging does.
    a.changeInfo(2, {});
    await until(() => battery() === null);
    a.changeInfo(2, { battery: { level: 4, charging: false } });
    await until(() => battery()?.percent === 4);
    a.changeInfo(2, { battery: { level: 4, charging: true } });
    await until(() => battery()?.charging === true);
    expect(deps.lowBattery).toHaveBeenCalledTimes(2);
    a.changeInfo(2, { battery: { level: 4, charging: false } });
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

  it("refuses device changes before sending while the adapter isn't ready", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", devices: [device(1), device(2, { state: "connected" })] });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 2));
    a.changeAdapter({ ready: false });
    await until(() => state()!.adapters[0]!.readiness === "waiting");
    const sent = a.received.length;
    const notReady = { ok: false, message: codeText("not_ready") };
    expect(await c.act({ type: "device.connect", key: "AAAA0001/1" })).toEqual(notReady);
    expect(await c.act({ type: "device.unpair", key: "AAAA0001/1" })).toEqual(notReady);
    expect(await c.act({ type: "device.update", key: "AAAA0001/1", trusted: false })).toEqual(notReady);
    expect(await c.act({ type: "settings.save", key: "AAAA0001/2", changes: [] })).toEqual(notReady);
    expect(a.received.length).toBe(sent);
    expect(await c.act({ type: "device.refresh", key: "AAAA0001/2" })).toEqual(notReady);
    expect(await c.act({ type: "device.disconnect", key: "AAAA0001/2" })).toEqual(notReady);
    await c.stop();
  });

  it("predicts the enabled-device limit only for changes that leave an unused device eligible", async () => {
    const a = new FakeAdapter({
      adapterId: "AAAA0001",
      maxEnabled: 1,
      devices: [device(1), device(2, { enabled: false, blocked: true }), device(3)],
    });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const full = { ok: false, message: "Every enabled-device place is in use. Turn off another device first." };
    const find = (id: number) => state()!.devices.find((d) => d.key === `AAAA0001/${id}`)!.device;
    expect(find(3).inactive).toBe("capacity");
    const sent = a.received.length;
    // Unblocking an enabled device and changing a device left out for capacity need a place.
    expect(await c.act({ type: "device.update", key: "AAAA0001/2", enabled: true, blocked: false })).toEqual(full);
    expect(await c.act({ type: "device.update", key: "AAAA0001/3", trusted: false })).toEqual(full);
    expect(a.received.length).toBe(sent);
    // Turning on a blocked device leaves it unused, and turning off one left out for capacity frees nothing.
    expect(await c.act({ type: "device.update", key: "AAAA0001/2", enabled: true })).toEqual({ ok: true });
    await until(() => find(2).enabled);
    expect(await c.act({ type: "device.update", key: "AAAA0001/2", blocked: false })).toEqual(full);
    expect(await c.act({ type: "device.update", key: "AAAA0001/3", enabled: false })).toEqual({ ok: true });
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
      message: "Every enabled-device place is in use. Turn off another device first.",
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
      message: "Every enabled-device place is in use. Turn off another device first.",
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

  it("keeps a failed scan's candidates and reports its failure with the related one", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    a.scanError = {
      code: ErrorCode.INTERNAL,
      reason: ErrorReason.RADIO,
      outcome: ErrorOutcome.PARTIAL,
      related: [{ code: ErrorCode.NO_CAPACITY, reason: ErrorReason.SCAN_CANDIDATES, outcome: ErrorOutcome.PARTIAL }],
    };
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.running === false);
    expect(state()!.scan).toMatchObject({
      candidates: [{ id: 1 }, { id: 2 }, { id: 3 }, { id: 4 }],
      error:
        "The adapter hit an unexpected failure. This involves the adapter's Bluetooth radio. The adapter returned only some of the results. Also, the adapter had no room to list every device it found. The adapter returned only some of the results.",
      failure: {
        code: "internal",
        reason: "radio",
        outcome: "partial",
        related: [{ code: "no_capacity", reason: "scan_candidates", outcome: "partial", related: [] }],
      },
    });
    // The candidates stay usable, and searching again clears the failure.
    expect(await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 1 })).toEqual({ ok: true });
    await c.act({ type: "pair.cancel" });
    await c.act({ type: "pair.dismiss" });
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    expect(c.state().scan).toMatchObject({ running: true, error: null, failure: null });
    await c.act({ type: "scan.stop" });
    await c.stop();
  });

  it("reports a pairing whose saved bond is unknown as failed, with its cleanup failure", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    a.pairingError = {
      code: ErrorCode.CANCELLED,
      outcome: ErrorOutcome.UNKNOWN,
      related: [{ code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.NO_EFFECT }],
    };
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 1 });
    await until(() => state()!.pairing?.phase === "failed");
    expect(state()!.pairing!.message).toBe(
      "The operation was cancelled. The adapter couldn't confirm whether it saved the device. Check its saved devices before pairing again. Also, the adapter couldn't read or write its saved data.",
    );
    expect(state()!.pairing!.failure).toEqual({
      code: "cancelled",
      reason: "unknown",
      outcome: "unknown",
      related: [{ code: "storage_failed", reason: "storage", outcome: "no_effect", related: [] }],
    });
    // Nothing is retried.
    expect(commands(a).filter((x) => x === "startPairing")).toHaveLength(1);
    await c.stop();
  });

  it("keeps a confirmed bond save and cleanup uncertainty apart, and never words either as a plain cancel", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", enabled: { classic: true } });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    a.pairingError = { code: ErrorCode.CANCELLED, outcome: ErrorOutcome.COMPLETE };
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 1 });
    await until(() => state()!.pairing?.phase === "failed");
    expect(state()!.pairing!.message).toBe("The operation was cancelled. The adapter saved the device despite this.");
    await c.act({ type: "pair.dismiss" });
    a.pairingError = {
      code: ErrorCode.AUTH_FAILED,
      outcome: ErrorOutcome.NO_EFFECT,
      related: [{ code: ErrorCode.INTERNAL, reason: ErrorReason.RADIO, outcome: ErrorOutcome.UNKNOWN }],
    };
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: 2 });
    await until(() => state()!.pairing?.candidateId === 2 && state()!.pairing?.phase === "failed");
    expect(state()!.pairing!.message).toBe(
      "Bluetooth authentication with this device failed. Also, the adapter hit an unexpected failure. This involves the adapter's Bluetooth radio. The adapter couldn't confirm whether it finished cleaning up.",
    );
    await c.stop();
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
    await until(() => state()!.adapters[0]?.connection === "reconnecting");
    expect(state()!.scan).toBeNull();
    expect(state()!.pairing).toMatchObject({ phase: "failed", prompt: null });
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
  expect(status().transports).toMatchObject([
    { transport: "classic", enabled: false },
    { transport: "ble", enabled: true },
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
  a.failures.setAdapter = [{ code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.NO_EFFECT }];
  expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", transports: { ble: true } })).toEqual({
    ok: false,
    message: "The adapter couldn't save the change.",
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
  const a = new FakeAdapter({ adapterId: "AAAA0001", board: "esp32s3", transports: ["ble"] });
  const { c, state } = controller({ "/a": a });
  await c.manager.rescan();
  await until(() => loaded(state(), 4));
  expect(state()!.adapters[0]!.status!.transports).toMatchObject([{ transport: "ble", enabled: true }]);
  const sent = a.received.length;
  expect(await c.act({ type: "adapter.settings", adapterId: "AAAA0001", transports: { classic: true } })).toEqual({
    ok: false,
    message: "The adapter or device doesn't support this action.",
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
  expect(state()!.adapters[0]!.status!.transports).toMatchObject([
    { transport: "classic", enabled: true },
    { transport: "ble", enabled: true },
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
