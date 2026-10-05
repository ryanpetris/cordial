import { ErrorCode } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { FakeAdapter, device, setting } from "../src/fake/adapter.ts";
import type { AppState } from "../src/shared/state.ts";
import { controller, until } from "./helpers.ts";

const KEY = "AAAA0001/1";

function keyboard(state: "connected" | "disconnected" = "connected") {
  return new FakeAdapter({
    adapterId: "AAAA0001",
    devices: [
      device(1, {
        state,
        hidppEnabled: true,
        hidpp: [4, 5],
        settings: [
          setting("backlight.enabled", { value: true }),
          setting("backlight.level", { type: "integer", min: 0, max: 7, step: 1, value: 3 }),
          setting("backlight.mode", { type: "enum", choices: ["automatic", "permanent_manual"], value: "automatic", saved: "automatic", state: "applied" }),
        ],
      }),
    ],
  });
}

async function start(fake: FakeAdapter) {
  const harness = controller({ "/a": fake });
  await harness.c.manager.rescan();
  await until(() => !!harness.state()?.devices[0]?.settings);
  const entry = () => harness.state()!.devices[0]!;
  const row = (key: string) => entry().settings!.find((s) => s.key === key)!;
  return { ...harness, entry, row };
}

const sent = (fake: FakeAdapter) => fake.received.filter((r) => r.command.case === "setSettings");

describe("settings form submission", () => {
  it("saves values and forgets others in one write, then follows the apply", async () => {
    const fake = keyboard();
    const { c, row } = await start(fake);
    const result = await c.act({
      type: "settings.save",
      key: KEY,
      changes: [
        { type: "set", setting: "backlight.level", value: 5 },
        { type: "forget", setting: "backlight.mode" },
        { type: "set", setting: "backlight.enabled", value: false },
      ],
    });
    expect(result).toMatchObject({ ok: true, settingsSave: { running: false } });
    expect(result.settingsSave!.items.map((i) => i.status)).toEqual(["saved", "saved", "saved"]);
    expect(sent(fake)).toHaveLength(1);
    expect(sent(fake)[0]!.command.value).toMatchObject({
      device: 1,
      changes: [
        { integration: 1, key: "backlight.level", change: { case: "value", value: { value: { case: "integer", value: 5n } } } },
        { integration: 1, key: "backlight.mode", change: { case: "forget" } },
        { integration: 1, key: "backlight.enabled", change: { case: "value", value: { value: { case: "bool", value: false } } } },
      ],
    });
    await until(() => row("backlight.level").state === "applied");
    expect(row("backlight.level")).toMatchObject({ value: 5, saved: 5 });
    expect(row("backlight.mode")).toMatchObject({ saved: null, state: null });
    await c.stop();
  });

  it("saves while the device is disconnected; the value waits to be applied", async () => {
    const fake = keyboard("disconnected");
    const { c, row, entry } = await start(fake);
    expect(entry().device.hidpp?.state).toBe("disconnected");
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: "backlight.mode", value: "permanent_manual" }] })).toMatchObject({ ok: true });
    await until(() => row("backlight.mode").saved === "permanent_manual");
    // A disconnected device's list holds its saved settings, without readings.
    expect(row("backlight.mode")).toMatchObject({ value: null, state: "pending" });
    expect(entry().settings!.map((s) => s.key)).toEqual(["backlight.mode"]);
    fake.changeDevice(1, { state: "connected" });
    await until(() => row("backlight.mode").state === "applied");
    expect(row("backlight.mode").value).toBe("permanent_manual");
    await c.stop();
  });

  it("shows a refused save without retrying", async () => {
    const fake = keyboard();
    const { c, row, state } = await start(fake);
    fake.failures.setSettings = [ErrorCode.STORAGE_FAILED];
    const result = await c.act({
      type: "settings.save",
      key: KEY,
      changes: [
        { type: "set", setting: "backlight.level", value: 5 },
        { type: "forget", setting: "backlight.mode" },
      ],
    });
    expect(result).toMatchObject({ ok: false, inline: true, message: expect.stringContaining("couldn't save") });
    expect(result.settingsSave!.items.map((i) => i.status)).toEqual(["not_saved", "not_saved"]);
    expect(sent(fake)).toHaveLength(1);
    expect(row("backlight.level").saved).toBeNull();
    await new Promise((r) => setTimeout(r, 50));
    expect(sent(fake)).toHaveLength(1);
    expect((state() as AppState).devices[0]!.settingsSave!.items[0]!.error).toMatch(/couldn't save/);
    await c.stop();
  });

  it("shows a value that saved but didn't apply", async () => {
    const fake = keyboard();
    fake.settingFailures["backlight.level"] = ErrorCode.DEVICE_ERROR;
    const { c, row } = await start(fake);
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: "backlight.level", value: 6 }] })).toMatchObject({ ok: true });
    await until(() => row("backlight.level").error === "device_error");
    expect(row("backlight.level")).toMatchObject({ saved: 6, value: 3, state: null });
    // Saving the value again applies it again.
    delete fake.settingFailures["backlight.level"];
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: "backlight.level", value: 6 }] })).toMatchObject({ ok: true });
    await until(() => row("backlight.level").state === "applied");
    await c.stop();
  });

  it("sends changes to one setting in order, and refuses unknown settings without sending anything", async () => {
    const fake = keyboard();
    const { c, row } = await start(fake);
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: "wheel.mode", value: "ratchet" }] })).toMatchObject({ ok: false });
    expect(sent(fake)).toHaveLength(0);
    expect(await c.act({
      type: "settings.save",
      key: KEY,
      changes: [
        { type: "forget", setting: "backlight.level" },
        { type: "set", setting: "backlight.level", value: 1 },
      ],
    })).toMatchObject({ ok: true });
    // The later change replaces the earlier one.
    await until(() => row("backlight.level").saved === 1);
    await c.stop();
  });

  it("forgets a submission's outcome with its adapter session", async () => {
    const fake = keyboard();
    const { c, deps, entry, state } = await start(fake);
    fake.failures.setSettings = [ErrorCode.BUSY];
    await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: "backlight.level", value: 2 }] });
    await until(() => !!entry().settingsSave);
    await c.act({ type: "adapter.disconnect", adapterId: fake.id });
    await c.act({ type: "adapter.connect", adapterId: fake.id });
    await until(() => !!state()?.devices[0]?.settings && state()!.devices[0]!.settingsSave === null);
    expect(deps.log).not.toHaveBeenCalledWith(expect.stringContaining("failed"));
    await c.stop();
  });
});
