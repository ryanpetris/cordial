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
    expect(session.status).toMatchObject({ id: fake.id, name: "Pico W", platform: "linux", ready: true });
    expect(session.status.transports).toEqual([
      { transport: "classic", maxEnabled: 7 },
      { transport: "ble", maxEnabled: 7 },
    ]);
    await until(() => session.listed && session.settings.size === 4);
    expect([...session.devices.keys()]).toEqual(["d_1", "d_2", "d_3", "d_4"]);
    expect(fake.received.map((r) => r.command.case)).toEqual([
      "getStatus", "listDevices",
      "listWarnings", "listSettings", "listWarnings", "listSettings", "listWarnings", "listSettings", "listWarnings", "listSettings",
    ]);
    expect(session.devices.get("d_4")).toMatchObject({ enabled: false, inactive: "disabled" });
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
    const read = session.connection.getDevice("d_3");
    // The adapter answers, then reports a later change.
    fake.changeDevice("d_3", { name: "Renamed" });
    expect((await read).name).toBe("Travel Keyboard");
    await until(() => session.devices.get("d_3")!.name === "Renamed");
    await session.connection.getStatus();
    expect(session.devices.get("d_3")!.name).toBe("Renamed");
    await session.close();
  });

  it("reads a newly paired device's lists and drops a removed device", async () => {
    const { fake, session } = await openSession({ devices: [device("d_1")] });
    await until(() => session.settings.size === 1);
    const added = device("d_9", { settings: [setting("wheel.invert", { value: true })] });
    fake.devices.push(added);
    fake.changeDevice("d_9", {});
    await until(() => !!session.settings.get("d_9")?.length && session.warnings.has("d_9"));
    expect(session.settings.get("d_9")![0]).toMatchObject({ key: "wheel.invert", type: "bool", value: true, saved: null, state: null });
    expect(await session.connection.unpairDevice("d_9")).toBeUndefined();
    await until(() => !session.devices.has("d_9"));
    expect(session.settings.has("d_9") || session.warnings.has("d_9")).toBe(false);
    await session.close();
  });

  it("converts settings with their limits, saved values and outcomes", async () => {
    const fake = new FakeAdapter({
      devices: [
        device("d_1", {
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
    await until(() => !!session.settings.get("d_1")?.length);
    expect(session.settings.get("d_1")).toEqual([
      { integration: 1, key: "backlight.level", type: "integer", value: 3, saved: 5, state: "changed_on_device", error: null, choices: [], min: 0, max: 7, step: 1, maxBytes: null },
      { integration: 1, key: "pointer.sensor.1.dpi", type: "integer", value: 800, saved: null, state: null, error: null, choices: [400, 800], min: null, max: null, step: null, maxBytes: null },
      { integration: 1, key: "wheel.mode", type: "enum", value: "ratchet", saved: "freespin", state: null, error: "timeout", choices: ["freespin", "ratchet"], min: null, max: null, step: null, maxBytes: null },
    ]);
    const entry = () => ({ device: session.devices.get("d_1")! });
    expect(settingsCurrent(entry())).toBe(true);
    expect(versionText(entry().device.hidpp!)).toBe("4.2");
    fake.changeDevice("d_1", { hidppState: IntegrationState.STARTING });
    await until(() => session.devices.get("d_1")!.hidpp?.state === "starting");
    expect(settingsCurrent(entry())).toBe(false);
    expect(integrationText(entry().device.hidpp!)).toBe("Setting Up");
    fake.changeDevice("d_1", { hidppState: null, hidppError: 50 });
    await until(() => session.devices.get("d_1")!.hidpp?.error === "protocol_unsupported");
    expect(integrationText(entry().device.hidpp!)).toBe("Failed: Not supported");
    fake.changeDevice("d_1", { hidppError: null, state: "disconnected" });
    await until(() => session.devices.get("d_1")!.hidpp?.state === "disconnected");
    // The readings stay, possibly out of date.
    expect(session.settings.get("d_1")![0]!.value).toBe(3);
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
