import { ErrorCode, ErrorOutcome, IntegrationState, OutputMode, Role, SettingsGroup } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { FakeAdapter, device, setting } from "../src/fake/adapter.ts";
import * as convert from "../src/core/convert.ts";
import { AdapterSession } from "../src/core/session.ts";
import { sameField, settingsCurrent } from "../src/shared/settings.ts";
import { integrationText, versionText } from "../src/shared/text.ts";
import { field, openSession, until } from "./helpers.ts";
import { vi } from "vitest";

describe("AdapterSession", () => {
  it("reads the status, then every device with its settings", async () => {
    const { fake, session } = await openSession();
    // Both transports are supported; Classic starts disabled and BLE enabled, as in the firmware.
    expect(session.status).toMatchObject({ id: fake.id, name: "Pico 2 W", platform: "linux", ready: true });
    const usage = (type: string, count: number | null, limit: number | null) => ({ type, count, limit });
    expect(session.status.transports).toEqual([
      { transport: "classic", enabled: false, resources: [usage("devices", 1, 7), usage("connections", 0, 7)] },
      { transport: "ble", enabled: true, resources: [usage("devices", 2, 7), usage("connections", 2, 7)] },
    ]);
    expect(session.status.errors).toEqual([]);
    expect(session.status.resources).toEqual([usage("interfaces", 11, 16), usage("storage", 24576, 524288)]);
    expect(session.status.information.storageFormat).toBe(2);
    // Limit-only entries keep their count unknown.
    expect(session.status.profileSupport).toEqual({ resources: [usage("memory", 0, 4096), usage("outputs", null, 8), usage("layers", null, 4)] });
    expect(session.status.interfaces).toEqual([
      { interface: 1, enabled: false, active: false, profile: 0, conflicts: [2] },
      { interface: 2, enabled: false, active: false, profile: 0, conflicts: [1] },
    ]);
    await until(() => session.listed && session.settings.size === 4);
    expect([...session.devices.keys()]).toEqual([1, 2, 3, 4]);
    expect(fake.received.map((r) => r.command.case)).toEqual([
      "getStatus", "listDevices", "listProfiles",
      "listSettings", "listSettings", "listSettings", "listSettings",
    ]);
    expect(session.devices.get(4)).toMatchObject({ enabled: false, inactive: "disabled", kinds: ["mouse"], profiles: [], errors: [] });
    await session.close();
  });

  it("reads every device in one response, keeping a device whose record can't be read", async () => {
    const devices = [1, 2, 3, 4, 5].map((id) => device(id));
    const { fake, session } = await openSession({ devices, batchSize: 2 });
    await until(() => session.listed);
    const lists = () => fake.received.filter((r) => r.command.case === "listDevices").length;
    expect(lists()).toBe(1);
    expect([...session.devices.keys()]).toEqual([1, 2, 3, 4, 5]);
    // A listing again replaces the devices: one gone without an event leaves, and one that can't
    // be read stays as it was.
    fake.devices = fake.devices.filter((d) => d.id !== 2);
    fake.unreadableDevices.add(3);
    fake.changeAdapter({ ready: false });
    fake.changeAdapter({ ready: true });
    await until(() => lists() === 2 && !session.devices.has(2));
    expect([...session.devices.keys()].sort()).toEqual([1, 3, 4, 5]);
    expect([...session.readErrors]).toEqual([[3, { code: "storage_failed", reason: "storage", outcome: "no_effect", related: [] }]]);
    // Reloading the unreadable device lists the devices again; a fresh record clears its error.
    session.reload(3);
    await until(() => lists() === 3);
    expect(session.readErrors.has(3)).toBe(true);
    fake.unreadableDevices.clear();
    fake.changeDevice(3, { name: "Renamed" });
    await until(() => session.devices.get(3)!.name === "Renamed");
    expect(session.readErrors.size).toBe(0);
    await session.close();
  });

  it("keeps the ID and error of every record it couldn't read, with or without an earlier record", async () => {
    const devices = [1, 2, 3].map((id) => device(id));
    const fake = new FakeAdapter({ devices, batchSize: 2 });
    fake.unreadableDevices.add(2);
    const hooks = { changed: vi.fn(), closed: vi.fn(), event: vi.fn(), log: vi.fn() };
    const session = await AdapterSession.open(fake.open(), hooks);
    session.run();
    await until(() => session.listed);
    const error = { code: "storage_failed", reason: "storage", outcome: "no_effect", related: [] };
    expect([...session.devices.keys()]).toEqual([1, 3]);
    expect(session.unreadableDevices).toEqual([{ id: 2, error }]);
    // Listing again reads it; until then nothing of its current state is made up.
    fake.unreadableDevices.clear();
    session.relist();
    await until(() => session.devices.has(2));
    expect(session.unreadableDevices).toEqual([]);
    expect(session.readErrors.size).toBe(0);
    await session.close();
  });

  it("skips listing entries of a kind newer than this app", async () => {
    const fake = new FakeAdapter({ devices: [1, 2, 3, 4, 5].map((id) => device(id)), batchSize: 2 });
    fake.newerDevices = new Set([2, 4]);
    const hooks = { changed: vi.fn(), closed: vi.fn(), event: vi.fn(), log: vi.fn() };
    const session = await AdapterSession.open(fake.open(), hooks);
    session.run();
    await until(() => session.listed);
    expect([...session.devices.keys()]).toEqual([1, 3, 5]);
    await session.close();
  });

  it("applies nothing from a listing that ends in an error", async () => {
    const devices = [1, 2, 3, 4, 5].map((id) => device(id));
    const { fake, session } = await openSession({ devices, batchSize: 2 });
    await until(() => session.listed);
    fake.devices = fake.devices.filter((d) => d.id !== 1);
    fake.lateFailures.listDevices = [ErrorCode.STORAGE_FAILED];
    await expect(session.connection.listDevices()).rejects.toMatchObject({ code: ErrorCode.STORAGE_FAILED });
    expect([...session.devices.keys()]).toEqual([1, 2, 3, 4, 5]);
    // The session goes on, and the next listing applies.
    expect((await session.connection.listDevices()).entries).toHaveLength(4);
    expect([...session.devices.keys()]).toEqual([2, 3, 4, 5]);
    await session.close();
  });

  it("reads every settings object in the adapter's order and applies change events", async () => {
    const settings = [
      setting("wheel", "mode", { choices: ["free_spin", "stepped"], value: "stepped", saved: "free_spin", state: "applied" }),
      setting("backlight", "brightness", { value: 3 }),
      setting("pointer_sensor", "dpi", { index: 1, value: 800 }),
      setting("wheel", "invert", { value: false }),
      setting("pointer_sensor", "dpi", { index: 0, value: 400 }),
      setting("backlight", "enabled", { value: true }),
    ];
    const { fake, session } = await openSession({ devices: [device(1, { state: "connected", settings })], batchSize: 1 });
    await until(() => session.listed && session.settings.get(1)?.length === 6);
    const fields = () => session.settings.get(1)!.map((x) => [x.group, x.index, x.field]);
    expect(fields()).toEqual([
      ["backlight", null, "enabled"],
      ["backlight", null, "brightness"],
      ["pointer_sensor", 0, "dpi"],
      ["pointer_sensor", 1, "dpi"],
      ["wheel", null, "mode"],
      ["wheel", null, "invert"],
    ]);
    expect(fake.received.filter((r) => r.command.case === "listSettings")).toHaveLength(1);
    // Disconnecting leaves only the saved field: the event replaces its object and removes the others.
    fake.changeDevice(1, { state: "disconnected" });
    await until(() => session.settings.get(1)!.length === 1);
    expect(session.settings.get(1)).toMatchObject([{ group: "wheel", field: "mode", value: null, saved: "free_spin" }]);
    const event = fake.events.findLast((e) => e.case === "settingsChanged");
    expect(event?.case === "settingsChanged" && event.value.changed?.map((x) => x.group?.case)).toEqual(["wheel"]);
    expect(event?.case === "settingsChanged" && event.value.removed?.map((x) => [x.group, x.index])).toEqual([
      [SettingsGroup.BACKLIGHT, undefined],
      [SettingsGroup.POINTER_SENSOR, 0],
      [SettingsGroup.POINTER_SENSOR, 1],
    ]);
    await session.close();
  });

  it("replaces every field of a changed object, including saved values and their status", async () => {
    const enabled = setting("backlight", "enabled", { value: false, saved: false, state: "applied" });
    const brightness = setting("backlight", "brightness", { value: 0, saved: 0, state: "changed_on_device" });
    const thumbwheel = setting("thumbwheel", "invert", { value: true });
    const d = device(1, { state: "connected", settings: [enabled, brightness, thumbwheel] });
    const { session } = await openSession({ devices: [d] });
    await until(() => session.settings.get(1)?.length === 3);
    // False and zero readings and saved values are known, not missing.
    expect(session.settings.get(1)!.slice(0, 2)).toMatchObject([
      { field: "enabled", value: false, saved: false, state: "applied" },
      { field: "brightness", value: 0, saved: 0, state: "changed_on_device" },
    ]);
    // The device stops reporting brightness and the adapter forgets the saved Backlight.
    d.settings = [{ ...enabled, saved: null, value: null }, thumbwheel];
    await session.connection.refreshDevice(1);
    await until(() => session.settings.get(1)!.length === 2);
    expect(session.settings.get(1)).toMatchObject([
      { group: "backlight", field: "enabled", value: null, saved: null, state: null, error: null },
      { group: "thumbwheel", field: "invert", value: true },
    ]);
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
    // False and zero are saved as values.
    const backlight = field("backlight", "enabled");
    const autoOff = field("power", "auto_off");
    await session.connection.setSettings({
      device: 1,
      changes: [
        convert.settingsUpdate({ type: "set", setting: backlight, value: false }),
        convert.settingsUpdate({ type: "set", setting: autoOff, value: 0 }),
      ],
    });
    expect(session.settings.get(1)!.find((x) => sameField(x, backlight))).toMatchObject({ saved: false, state: "pending" });
    expect(session.settings.get(1)!.find((x) => sameField(x, autoOff))).toMatchObject({ saved: 0, state: "pending" });
    await session.connection.setSettings({ device: 1, changes: [convert.settingsUpdate({ type: "forget", setting: backlight })] });
    expect(session.settings.get(1)!.find((x) => sameField(x, backlight))).toMatchObject({ saved: null, state: null });
    const id = await session.connection.createProfile("Work");
    expect(session.profileNames.get(id)).toEqual({ id, name: "Work", roles: [] });
    expect(fake.received.slice(sent).map((r) => r.command.case)).toEqual(["setDevice", "setAdapter", "setSettings", "setSettings", "createProfile"]);
    // A saved disable or block also leaves the reason the device is inactive.
    await session.connection.setDevice({ device: 1, enabled: false });
    expect(session.devices.get(1)).toMatchObject({ enabled: false, inactive: "disabled" });
    await session.connection.setDevice({ device: 1, blocked: true });
    expect(session.devices.get(1)).toMatchObject({ blocked: true, inactive: "blocked" });
    // Output updates apply in order, and Disabled leaves no saved entry.
    const gamepad = (...modes: OutputMode[]) => modes.map((mode) => ({ role: Role.GAMEPAD, mode }));
    await session.connection.setDevice({ device: 2, outputPreferences: gamepad(OutputMode.XINPUT, OutputMode.DISABLED) });
    expect(session.devices.get(2)!.outputPreferences).toEqual([]);
    await session.connection.setDevice({ device: 2, outputPreferences: gamepad(OutputMode.XINPUT, OutputMode.HID) });
    expect(session.devices.get(2)!.outputPreferences).toEqual([{ role: "gamepad", mode: "hid" }]);
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

  it("reads a newly paired device's settings and drops a removed device", async () => {
    const { fake, session } = await openSession({ devices: [device(1)] });
    await until(() => session.settings.size === 1);
    const added = device(9, { state: "connected", settings: [setting("wheel", "invert", { value: true })] });
    fake.devices.push(added);
    fake.changeDevice(9, {});
    await until(() => !!session.settings.get(9)?.length);
    expect(session.settings.get(9)![0]).toMatchObject({ group: "wheel", field: "invert", type: "bool", value: true, saved: null, state: null });
    expect(await session.connection.unpairDevice(9)).toBeUndefined();
    await until(() => !session.devices.has(9));
    expect(session.settings.has(9)).toBe(false);
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
            setting("backlight", "brightness", { min: 0, max: 7, step: 1, value: 3, saved: 5, state: "changed_on_device" }),
            setting("pointer_sensor", "dpi", { index: 1, choices: [400, 800], value: 800 }),
            setting("wheel", "mode", { choices: ["free_spin", "stepped"], value: "stepped", saved: "free_spin", error: ErrorCode.TIMEOUT }),
          ],
        }),
      ],
    });
    const session = await AdapterSession.open(fake.open(), { changed: vi.fn(), closed: vi.fn(), event: vi.fn(), log: vi.fn() });
    session.run();
    await until(() => !!session.settings.get(1)?.length);
    expect(session.settings.get(1)).toEqual([
      { integration: 1, group: "backlight", index: null, field: "brightness", type: "integer", value: 3, saved: 5, state: "changed_on_device", error: null, choices: [], min: 0, max: 7, step: 1 },
      { integration: 1, group: "pointer_sensor", index: 1, field: "dpi", type: "integer", value: 800, saved: null, state: null, error: null, choices: [400, 800], min: null, max: null, step: null },
      { integration: 1, group: "wheel", index: null, field: "mode", type: "enum", value: "stepped", saved: "free_spin", state: null, error: { code: "timeout", reason: "unknown", outcome: "unspecified", related: [] }, choices: ["free_spin", "stepped"], min: null, max: null, step: null },
    ]);
    const entry = () => ({ device: session.devices.get(1)!, readError: null, available: session.available, settingsAvailable: session.settingsAvailable.has(1) });
    expect(settingsCurrent(entry())).toBe(true);
    expect(versionText(entry().device.hidpp!)).toBe("4.2");
    fake.changeDevice(1, { hidppState: IntegrationState.STARTING });
    await until(() => session.devices.get(1)!.hidpp?.state === "starting");
    expect(settingsCurrent(entry())).toBe(false);
    expect(integrationText(entry().device.hidpp!)).toBe("Setting Up");
    fake.changeDevice(1, { hidppState: null, hidppError: ErrorCode.PROTOCOL_UNSUPPORTED });
    await until(() => session.devices.get(1)!.hidpp?.error?.code === "protocol_unsupported");
    expect(integrationText(entry().device.hidpp!)).toBe("Unsupported");
    fake.changeDevice(1, { hidppError: { code: ErrorCode.TIMEOUT, outcome: ErrorOutcome.UNKNOWN, related: [{ code: ErrorCode.TRANSPORT_ERROR }] } });
    await until(() => session.devices.get(1)!.hidpp?.error?.code === "timeout");
    expect(integrationText(entry().device.hidpp!)).toBe(
      "Failed: This device didn't respond in time. The adapter couldn't confirm whether the change took effect. Also, the adapter couldn't exchange messages with this device",
    );
    // While disconnected, the list holds the saved settings, with their limits and without readings.
    fake.changeDevice(1, { hidppError: null, state: "disconnected" });
    await until(() => session.devices.get(1)!.hidpp?.state === "disconnected" && session.settings.get(1)!.length === 2);
    expect(session.settings.get(1)!.map((s) => [s.field, s.value, s.saved, s.max])).toEqual([["brightness", null, 5, 7], ["mode", null, "free_spin", null]]);
    expect(session.devices.get(1)!.information).toEqual(convert.information(undefined));
    expect(settingsCurrent(entry())).toBe(false);
    await session.close();
  });

  it("lists the devices once the adapter becomes ready", async () => {
    const { fake, session } = await openSession({ ready: false });
    await new Promise((r) => setTimeout(r, 50));
    expect(session.available).toBe(false);
    expect(fake.received.map((r) => r.command.case)).toEqual(["getStatus"]);
    fake.changeAdapter({ ready: true });
    await until(() => session.listed);
    expect(fake.received.filter((r) => r.command.case === "listDevices")).toHaveLength(1);
    await session.close();
  });

  it("follows storage usage the adapter reports in its events", async () => {
    const { fake, session } = await openSession();
    await until(() => session.listed);
    fake.changeAdapter({ storageUsed: 0, storageTotal: null });
    await until(() => session.status.resources.some((r) => r.type === "storage" && r.count === 0));
    expect(session.status.resources.find((r) => r.type === "storage")).toEqual({ type: "storage", count: 0, limit: null });
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
