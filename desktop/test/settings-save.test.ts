import { ErrorCode } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { isAction } from "../src/core/actions.ts";
import { FakeAdapter, device, setting } from "../src/fake/adapter.ts";
import { sameField } from "../src/shared/settings.ts";
import type { AppState, SettingFields, SettingsGroupName } from "../src/shared/state.ts";
import { controller, field, until } from "./helpers.ts";

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
          setting("backlight", "enabled", { value: true }),
          setting("backlight", "brightness", { min: 0, max: 7, step: 1, value: 3 }),
          setting("backlight", "mode", { choices: ["automatic", "permanent_manual"], value: "automatic", saved: "automatic", state: "applied" }),
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
  const row = <G extends SettingsGroupName>(group: G, name: SettingFields[G]) => entry().settings!.find((s) => sameField(s, field(group, name)))!;
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
        { type: "set", setting: field("backlight", "brightness"), value: 5 },
        { type: "forget", setting: field("backlight", "mode") },
        { type: "set", setting: field("backlight", "enabled"), value: false },
      ],
    });
    expect(result).toMatchObject({ ok: true, settingsSave: { running: false } });
    expect(result.settingsSave!.items.map((i) => i.status)).toEqual(["saved", "saved", "saved"]);
    expect(sent(fake)).toHaveLength(1);
    expect(sent(fake)[0]!.command.value).toMatchObject({
      device: 1,
      changes: [
        { integration: 1, group: { case: "backlight", value: { brightness: { change: { case: "value", value: 5n } } } } },
        { integration: 1, group: { case: "backlight", value: { mode: { change: { case: "forget" } } } } },
        { integration: 1, group: { case: "backlight", value: { enabled: { change: { case: "value", value: false } } } } },
      ],
    });
    await until(() => row("backlight", "brightness").state === "applied");
    expect(row("backlight", "brightness")).toMatchObject({ value: 5, saved: 5 });
    expect(row("backlight", "mode")).toMatchObject({ saved: null, state: null });
    await c.stop();
  });

  it("saves while the device is disconnected; the value waits to be applied", async () => {
    const fake = keyboard("disconnected");
    const { c, row, entry } = await start(fake);
    expect(entry().device.hidpp?.state).toBe("disconnected");
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("backlight", "mode"), value: "permanent_manual" }] })).toMatchObject({ ok: true });
    await until(() => row("backlight", "mode").saved === "permanent_manual");
    // A disconnected device's list holds its saved settings, without readings.
    expect(row("backlight", "mode")).toMatchObject({ value: null, state: "pending" });
    expect(entry().settings!.map((s) => s.field)).toEqual(["mode"]);
    fake.changeDevice(1, { state: "connected" });
    await until(() => row("backlight", "mode").state === "applied");
    expect(row("backlight", "mode").value).toBe("permanent_manual");
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
        { type: "set", setting: field("backlight", "brightness"), value: 5 },
        { type: "forget", setting: field("backlight", "mode") },
      ],
    });
    expect(result).toMatchObject({ ok: false, inline: true, message: expect.stringContaining("couldn't save") });
    expect(result.settingsSave!.items.map((i) => i.status)).toEqual(["not_saved", "not_saved"]);
    expect(sent(fake)).toHaveLength(1);
    expect(row("backlight", "brightness").saved).toBeNull();
    await new Promise((r) => setTimeout(r, 50));
    expect(sent(fake)).toHaveLength(1);
    expect((state() as AppState).devices[0]!.settingsSave!.items[0]!.error).toMatch(/couldn't save/);
    await c.stop();
  });

  it("shows a save whose outcome the adapter couldn't confirm as unknown", async () => {
    const fake = keyboard();
    const { c } = await start(fake);
    fake.failures.setSettings = [ErrorCode.STORAGE_FAILED];
    fake.uncertainWrites = true;
    const result = await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("backlight", "brightness"), value: 5 }] });
    expect(result).toMatchObject({ ok: false, inline: true, message: "The adapter couldn't confirm whether the change was saved. Check before trying again." });
    expect(result.settingsSave!.items.map((i) => i.status)).toEqual(["unknown"]);
    await c.stop();
  });

  it("shows a save the adapter disconnected during as unknown", async () => {
    const fake = keyboard();
    const { c } = await start(fake);
    const release = fake.hold();
    const pending = c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("backlight", "brightness"), value: 5 }] });
    await until(() => sent(fake).length === 1);
    fake.unplug();
    release();
    const result = await pending;
    expect(result).toMatchObject({ ok: false, message: "The adapter couldn't confirm whether the change was saved." });
    expect(result.settingsSave!.items.map((i) => i.status)).toEqual(["unknown"]);
    await c.stop();
  });

  it("shows a value that saved but didn't apply", async () => {
    const fake = keyboard();
    const brightness = fake.devices[0]!.settings.find((s) => s.field === "brightness")!;
    brightness.fails = ErrorCode.DEVICE_ERROR;
    const { c, row } = await start(fake);
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("backlight", "brightness"), value: 6 }] })).toMatchObject({ ok: true });
    await until(() => row("backlight", "brightness").error === "device_error");
    expect(row("backlight", "brightness")).toMatchObject({ saved: 6, value: 3, state: null });
    // Saving the value again applies it again.
    brightness.fails = null;
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("backlight", "brightness"), value: 6 }] })).toMatchObject({ ok: true });
    await until(() => row("backlight", "brightness").state === "applied");
    await c.stop();
  });

  it("sends changes to one setting in order, and refuses unknown settings and values without sending anything", async () => {
    const fake = keyboard();
    const { c, row } = await start(fake);
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("wheel", "mode"), value: "stepped" }] })).toMatchObject({ ok: false });
    expect(await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("backlight", "enabled"), value: 1 }] })).toMatchObject({ ok: false });
    expect(sent(fake)).toHaveLength(0);
    expect(await c.act({
      type: "settings.save",
      key: KEY,
      changes: [
        { type: "forget", setting: field("backlight", "brightness") },
        { type: "set", setting: field("backlight", "brightness"), value: 1 },
      ],
    })).toMatchObject({ ok: true });
    // The later change replaces the earlier one.
    await until(() => row("backlight", "brightness").saved === 1);
    await c.stop();
  });

  it("forgets a submission's outcome with its adapter session", async () => {
    const fake = keyboard();
    const { c, deps, entry, state } = await start(fake);
    fake.failures.setSettings = [ErrorCode.BUSY];
    await c.act({ type: "settings.save", key: KEY, changes: [{ type: "set", setting: field("backlight", "brightness"), value: 2 }] });
    await until(() => !!entry().settingsSave);
    await c.act({ type: "adapter.disconnect", adapterId: fake.id });
    await c.act({ type: "adapter.connect", adapterId: fake.id });
    await until(() => !!state()?.devices[0]?.settings && state()!.devices[0]!.settingsSave === null);
    expect(deps.log).not.toHaveBeenCalledWith(expect.stringContaining("failed"));
    await c.stop();
  });

  it("accepts only known setting fields from the window", () => {
    const save = (setting: unknown, value: unknown = 1) => isAction({ type: "settings.save", key: KEY, changes: [{ type: "set", setting, value }] });
    expect(save(field("power", "auto_off"), 0)).toBe(true);
    expect(save(field("pointer_sensor", "dpi", 0))).toBe(true);
    expect(save("power.auto_off")).toBe(false);
    expect(save({ ...field("power", "auto_off"), field: "dpi" })).toBe(false);
    expect(save({ ...field("power", "auto_off"), group: "toString" })).toBe(false);
    expect(save(field("power", "auto_off", 0))).toBe(false);
    expect(save(field("pointer_sensor", "dpi"))).toBe(false);
    expect(save({ ...field("power", "auto_off"), key: "x" })).toBe(false);
  });
});
