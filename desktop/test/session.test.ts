import { IntegrationState } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { FakeAdapter, device, setting } from "../src/fake/adapter.ts";
import { AdapterSession } from "../src/core/session.ts";
import { settingsCurrent } from "../src/shared/settings.ts";
import { integrationText, versionText } from "../src/shared/text.ts";
import { openSession, until } from "./helpers.ts";
import { vi } from "vitest";

describe("AdapterSession", () => {
  it("reads the status, then every device with its warnings and settings", async () => {
    const { fake, session } = await openSession();
    // Both transports are supported; Classic starts disabled and BLE enabled, as in the firmware.
    expect(session.status).toMatchObject({ id: fake.id, name: "Pico 2 W", platform: "linux", ready: true });
    expect(session.status.transports).toEqual([
      { transport: "classic", maxEnabled: 7, enabled: false },
      { transport: "ble", maxEnabled: 7, enabled: true },
    ]);
    expect(session.status.profileSupport).toEqual({ memoryBudget: 4096, memoryUsed: 0, maxLayers: 4 });
    expect(session.status.interfaces).toEqual([
      { interface: 1, enabled: false, profile: 0, conflicts: [2] },
      { interface: 2, enabled: false, profile: 0, conflicts: [1] },
    ]);
    await until(() => session.listed && session.settings.size === 4);
    expect([...session.devices.keys()]).toEqual([1, 2, 3, 4]);
    expect(fake.received.map((r) => r.command.case)).toEqual([
      "getStatus", "listDevices", "listProfiles",
      "listWarnings", "listSettings", "listWarnings", "listSettings", "listWarnings", "listSettings", "listWarnings", "listSettings",
    ]);
    expect(session.devices.get(4)).toMatchObject({ enabled: false, inactive: "disabled", kinds: ["mouse"], profiles: [], profileError: null });
    await session.close();
  });

  it("reads every page of devices, keeping a device whose record can't be read", async () => {
    const devices = [1, 2, 3, 4, 5].map((id) => device(id));
    const { fake, session } = await openSession({ devices, pageSize: 2 });
    await until(() => session.listed);
    const afters = () => fake.received.flatMap((r) => (r.command.case === "listDevices" ? [r.command.value.after] : []));
    expect(afters()).toEqual([0, 2, 4]);
    expect([...session.devices.keys()]).toEqual([1, 2, 3, 4, 5]);
    // A listing again replaces each page's range: a device gone without an event leaves, and one
    // that can't be read stays as it was.
    fake.devices = fake.devices.filter((d) => d.id !== 2);
    fake.unreadableDevices.add(3);
    fake.changeAdapter({ ready: false });
    fake.changeAdapter({ ready: true });
    await until(() => afters().length === 5);
    expect(afters().slice(3)).toEqual([0, 3]);
    await until(() => !session.devices.has(2));
    expect([...session.devices.keys()].sort()).toEqual([1, 3, 4, 5]);
    await session.close();
  });

  it("reads every page of settings in the adapter's order and merges change events", async () => {
    const settings = ["wheel.mode", "backlight.level", "wheel.invert", "backlight.enabled"].map((key, i) => setting(key, { value: true, saved: i === 0 ? true : null, state: "applied" }));
    const { fake, session } = await openSession({ devices: [device(1, { state: "connected", settings })], pageSize: 1 });
    await until(() => session.listed && session.settings.has(1));
    expect(session.settings.get(1)!.map((x) => x.key)).toEqual(["backlight.enabled", "backlight.level", "wheel.invert", "wheel.mode"]);
    const afters = fake.received.flatMap((r) => (r.command.case === "listSettings" ? [r.command.value.after?.key] : []));
    expect(afters).toEqual([undefined, "backlight.enabled", "backlight.level", "wheel.invert"]);
    // Disconnecting leaves only the saved setting: the event removes the others and changes it.
    fake.changeDevice(1, { state: "disconnected" });
    await until(() => session.settings.get(1)!.length === 1);
    expect(session.settings.get(1)).toMatchObject([{ key: "wheel.mode", value: null, saved: true }]);
    const event = fake.events.findLast((e) => e.case === "settingsChanged");
    expect(event?.case === "settingsChanged" && event.value.changed?.map((x) => x.key)).toEqual(["wheel.mode"]);
    expect(event?.case === "settingsChanged" && event.value.removed?.map((x) => x.key)).toEqual(["backlight.enabled", "backlight.level", "wheel.invert"]);
    await session.close();
  });

  it("applies a change the adapter accepted without reading it back", async () => {
    const { fake, session } = await openSession();
    await until(() => session.listed && session.settings.size === 4);
    const sent = fake.received.length;
    await session.connection.setDevice({ device: 3, trusted: false, profiles: { profiles: [] } });
    expect(session.devices.get(3)).toMatchObject({ trusted: false, profiles: [] });
    await session.connection.setAdapter({ name: "Desk", platform: 2, transports: [{ transport: 1, enabled: true }] });
    expect(session.status).toMatchObject({ name: "Desk", platform: "mac" });
    expect(session.status.transports.find((t) => t.transport === "classic")!.enabled).toBe(true);
    const key = session.settings.get(1)!.find((x) => x.type === "bool")!.key;
    await session.connection.setSettings({ device: 1, changes: [{ integration: 1, key, change: { case: "value", value: { value: { case: "bool", value: false } } } }] });
    expect(session.settings.get(1)!.find((x) => x.key === key)).toMatchObject({ saved: false });
    const id = await session.connection.createProfile("Work");
    expect(session.profileNames.get(id)).toEqual({ id, name: "Work", roles: [] });
    expect(fake.received.slice(sent).map((r) => r.command.case)).toEqual(["setDevice", "setAdapter", "setSettings", "createProfile"]);
    // A saved disable or block also leaves the reason the device is inactive.
    await session.connection.setDevice({ device: 1, enabled: false });
    expect(session.devices.get(1)).toMatchObject({ enabled: false, inactive: "disabled" });
    await session.connection.setDevice({ device: 1, blocked: true });
    expect(session.devices.get(1)).toMatchObject({ blocked: true, inactive: "blocked" });
    await session.close();
  });

    it("keeps readiness reported in the same chunk as the first status", async () => {
    const fake = new FakeAdapter({ ready: false });
    const port = fake.open();
    // Hold the adapter's output and deliver it as one chunk, so the readiness event arrives
    // before open() returns.
    let held: Uint8Array[] = [];
    const listeners: ((chunk: Uint8Array) => void)[] = [];
    port.onData((chunk) => held.push(chunk));
    const flush = () => {
      const chunk = new Uint8Array(held.flatMap((c) => [...c]));
      held = [];
      for (const listener of listeners) listener(chunk);
    };
    const stream = {
      onData: (listener: (chunk: Uint8Array) => void) => listeners.push(listener),
      onClose: (listener: (error: Error | null) => void) => port.onClose(listener),
      close: () => port.close(),
      write: async (bytes: Uint8Array) => {
        const first = fake.received.length === 0;
        await port.write(bytes);
        if (first) fake.changeAdapter({ ready: true });
        setTimeout(flush, 10);
      },
    };
    const hooks = { changed: vi.fn(), closed: vi.fn(), event: vi.fn(), log: vi.fn() };
    const session = await AdapterSession.open(stream, hooks);
    expect(session.status.ready).toBe(true);
    session.run();
    await until(() => session.listed);
    expect(hooks.log).not.toHaveBeenCalled();
    await session.close();
  });

  it("applies responses and events in the order the adapter sent them", async () => {
    const { fake, session } = await openSession();
    await until(() => session.listed);
    const read = session.connection.getDevice(3);
    // The adapter answers, then reports a later change.
    fake.changeDevice(3, { name: "Renamed" });
    expect((await read).name).toBe("Travel Keyboard");
    await until(() => session.devices.get(3)!.name === "Renamed");
    await session.connection.getStatus();
    expect(session.devices.get(3)!.name).toBe("Renamed");
    await session.close();
  });

  it("reads a newly paired device's lists and drops a removed device", async () => {
    const { fake, session } = await openSession({ devices: [device(1)] });
    await until(() => session.settings.size === 1);
    const added = device(9, { state: "connected", settings: [setting("wheel.invert", { value: true })] });
    fake.devices.push(added);
    fake.changeDevice(9, {});
    await until(() => !!session.settings.get(9)?.length && session.warnings.has(9));
    expect(session.settings.get(9)![0]).toMatchObject({ key: "wheel.invert", type: "bool", value: true, saved: null, state: null });
    expect(await session.connection.unpairDevice(9)).toBeUndefined();
    await until(() => !session.devices.has(9));
    expect(session.settings.has(9) || session.warnings.has(9)).toBe(false);
    await session.close();
  });

  it("converts settings with their limits, saved values and outcomes", async () => {
    const fake = new FakeAdapter({
      devices: [
        device(1, {
          state: "connected",
          hidppEnabled: true,
          hidpp: [4, 2],
          settings: [
            setting("backlight.level", { type: "integer", min: 0, max: 7, step: 1, value: 3, saved: 5, state: "changed_on_device" }),
            setting("pointer.sensor.1.dpi", { type: "integer", choices: [400, 800], value: 800 }),
            setting("wheel.mode", { type: "enum", choices: ["freespin", "ratchet"], value: "ratchet", saved: "freespin", error: 33 }),
          ],
        }),
      ],
    });
    const session = await AdapterSession.open(fake.open(), { changed: vi.fn(), closed: vi.fn(), event: vi.fn(), log: vi.fn() });
    session.run();
    await until(() => !!session.settings.get(1)?.length);
    expect(session.settings.get(1)).toEqual([
      { integration: 1, key: "backlight.level", type: "integer", value: 3, saved: 5, state: "changed_on_device", error: null, choices: [], min: 0, max: 7, step: 1, maxBytes: null },
      { integration: 1, key: "pointer.sensor.1.dpi", type: "integer", value: 800, saved: null, state: null, error: null, choices: [400, 800], min: null, max: null, step: null, maxBytes: null },
      { integration: 1, key: "wheel.mode", type: "enum", value: "ratchet", saved: "freespin", state: null, error: "timeout", choices: ["freespin", "ratchet"], min: null, max: null, step: null, maxBytes: null },
    ]);
    const entry = () => ({ device: session.devices.get(1)! });
    expect(settingsCurrent(entry())).toBe(true);
    expect(versionText(entry().device.hidpp!)).toBe("4.2");
    fake.changeDevice(1, { hidppState: IntegrationState.STARTING });
    await until(() => session.devices.get(1)!.hidpp?.state === "starting");
    expect(settingsCurrent(entry())).toBe(false);
    expect(integrationText(entry().device.hidpp!)).toBe("Setting Up");
    fake.changeDevice(1, { hidppState: null, hidppError: 50 });
    await until(() => session.devices.get(1)!.hidpp?.error === "protocol_unsupported");
    expect(integrationText(entry().device.hidpp!)).toBe("Failed: Not supported");
    // While disconnected, the list holds the saved settings, with their limits and without readings.
    fake.changeDevice(1, { hidppError: null, state: "disconnected" });
    await until(() => session.devices.get(1)!.hidpp?.state === "disconnected" && session.settings.get(1)!.length === 2);
    expect(session.settings.get(1)!.map((s) => [s.key, s.value, s.saved, s.max])).toEqual([["backlight.level", null, 5, 7], ["wheel.mode", null, "freespin", null]]);
    expect(session.devices.get(1)!.info).toEqual([]);
    expect(settingsCurrent(entry())).toBe(false);
    await session.close();
  });

  it("lists the devices again once the adapter becomes ready", async () => {
    const { fake, session } = await openSession({ ready: false });
    await until(() => session.listed);
    expect(session.status.ready).toBe(false);
    fake.changeAdapter({ ready: true });
    await until(() => fake.received.filter((r) => r.command.case === "listDevices").length === 2);
    expect(session.status.ready).toBe(true);
    await session.close();
  });

  it("reports an unplugged adapter as closed", async () => {
    const { fake, session, hooks } = await openSession();
    await until(() => session.listed);
    fake.unplug();
    expect(hooks.closed).toHaveBeenCalledOnce();
    expect(session.closed).toBe(true);
  });

  it("rejects a port that doesn't answer as an adapter", async () => {
    const fake = new FakeAdapter();
    fake.write = async () => {};
    const close = vi.spyOn(fake, "close");
    await expect(AdapterSession.open(fake.open(), { changed: vi.fn(), closed: vi.fn(), event: vi.fn(), log: vi.fn() })).rejects.toThrow("no response");
    expect(close).toHaveBeenCalled();
  }, 10_000);
});
