import { describe, expect, it } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import type { SettingsChange } from "../src/shared/state.ts";
import { controller, until } from "./helpers.ts";

async function opened(fake: FakeAdapter) {
  const result = controller({ "/fake": fake });
  await result.c.manager.rescan();
  const key = `${fake.id}/d_1`;
  await result.c.act({ type: "settings.watch", key });
  await until(() => !!result.c.state().devices.find((d) => d.key === key)?.settings?.current);
  return { ...result, key };
}

const writes = (fake: FakeAdapter) => fake.received.filter((r) => r.cmd === "hidpp.setting.set" || r.cmd === "hidpp.setting.forget");
const keys = (fake: FakeAdapter) => writes(fake).map((r) => (r.args as { key: string }).key);

describe("settings form submission", () => {
  it("waits for hardware between keys and keeps observing after navigation", async () => {
    const fake = new FakeAdapter({ settingJobMs: 150 });
    const { c, key } = await opened(fake);
    try {
      const saving = c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "fn.row_default", value: "function_keys" },
        { type: "set", setting: "backlight.enabled", value: false },
      ] });
      await until(() => fake.find("d_1").device.settings_state === "applying");
      expect(c.state().devices.find((d) => d.key === key)?.settingsSave?.running).toBe(true);
      expect(keys(fake)).toEqual(["fn.row_default"]);
      expect(await c.act({ type: "settings.refresh", key })).toMatchObject({ ok: false });
      expect(await c.act({ type: "setting.set", key, setting: "backlight.enabled", value: true })).toMatchObject({ ok: false });
      expect(await c.act({ type: "device.hidpp", key, value: false })).toMatchObject({ ok: false });
      await c.act({ type: "settings.watch", key: `${fake.id}/d_2` });
      const result = await saving;
      expect(result.ok).toBe(true);
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["applied", "applied"]);
      expect(keys(fake)).toEqual(["fn.row_default", "backlight.enabled"]);
      expect(fake.received.some((r) => r.cmd === "hidpp.setting.apply")).toBe(false);
      expect(fake.find("d_1").settings.find((s) => s.key === "backlight.delay.hands_out")).toMatchObject({ desired: 30, observed: 60 });
    } finally { await c.stop(); }
  });

  it("stores every value with Logitech Features off without awaiting a hardware job", async () => {
    const fake = new FakeAdapter({ settingJobMs: 100 });
    const { c, key } = await opened(fake);
    try {
      fake.changeDevice("d_1", { hidpp_enabled: false, normalization_state: "off", settings_state: "off" });
      const result = await c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "backlight.level", value: 5 },
        { type: "set", setting: "backlight.mode", value: "permanent_manual" },
      ] });
      expect(result.ok).toBe(true);
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["saved", "saved"]);
      expect(keys(fake)).toEqual(["backlight.mode", "backlight.level"]);
      expect(fake.find("d_1").settings.find((s) => s.key === "backlight.level")).toMatchObject({ desired: 5, observed: 3, state: "pending" });
      expect(fake.find("d_1").settings.find((s) => s.key === "backlight.mode")).toMatchObject({ desired: "permanent_manual", observed: "automatic" });
    } finally { await c.stop(); }
  });

  it("continues independent edits after a rejected save and retains per-key outcomes", async () => {
    const fake = new FakeAdapter({ settingJobMs: 20 });
    const { c, key } = await opened(fake);
    try {
      fake.failures["hidpp.setting.set"] = ["", "storage_failed"];
      const result = await c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "fn.row_default", value: "function_keys" },
        { type: "set", setting: "backlight.delay.hands_out", value: 40 },
        { type: "set", setting: "backlight.enabled", value: false },
      ] });
      expect(result).toMatchObject({ ok: false, inline: true });
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["applied", "not_saved", "applied"]);
      expect(fake.find("d_1").settings.find((s) => s.key === "backlight.delay.hands_out")?.desired).toBe(30);
    } finally { await c.stop(); }
  });

  it("distinguishes saved-but-failed hardware work and retries only the selected key", async () => {
    const fake = new FakeAdapter({ settingJobMs: 20 });
    const { c, key } = await opened(fake);
    try {
      fake.settingFailures["fn.row_default"] = "readback_mismatch";
      const result = await c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "fn.row_default", value: "function_keys" },
        { type: "set", setting: "backlight.enabled", value: false },
      ] });
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["not_applied", "applied"]);
      expect(fake.find("d_1").settings.find((s) => s.key === "fn.row_default")).toMatchObject({ desired: "function_keys", observed: "special_actions" });
      const siblingStart = writes(fake).length;
      const sibling = await c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "backlight.delay.hands_out", value: 40 },
      ] });
      expect(keys(fake).slice(siblingStart)).toEqual(["backlight.delay.hands_out"]);
      expect(sibling.settingsSave?.items).toMatchObject([
        { change: { setting: "backlight.delay.hands_out" }, status: "applied" },
        { change: { setting: "fn.row_default" }, status: "not_applied" },
      ]);
      delete fake.settingFailures["fn.row_default"];
      const count = writes(fake).length;
      const retry = await c.act({ type: "settings.save", key, changes: [result.settingsSave!.items[0]!.change] });
      expect(retry.ok).toBe(true);
      expect(keys(fake).slice(count)).toEqual(["fn.row_default"]);
    } finally { await c.stop(); }
  });

  it("orders the mode before its dependent level and leaves the level unsent after failure", async () => {
    const fake = new FakeAdapter({ settingJobMs: 20 });
    const { c, key } = await opened(fake);
    try {
      fake.settingFailures["backlight.mode"] = "readback_mismatch";
      const result = await c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "backlight.level", value: 5 },
        { type: "set", setting: "backlight.mode", value: "permanent_manual" },
        { type: "set", setting: "backlight.enabled", value: false },
      ] });
      expect(keys(fake)).toEqual(["backlight.mode", "backlight.enabled"]);
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["not_applied", "not_sent", "applied"]);
      expect(fake.find("d_1").settings.find((s) => s.key === "backlight.level")?.managed).toBe(false);
    } finally { await c.stop(); }
  });

  it("stops on disconnect without losing the already stored preference", async () => {
    const fake = new FakeAdapter({ settingJobMs: 200 });
    const { c, key } = await opened(fake);
    try {
      const saving = c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "fn.row_default", value: "function_keys" },
        { type: "set", setting: "backlight.enabled", value: false },
      ] });
      await until(() => fake.find("d_1").device.settings_state === "applying");
      fake.changeDevice("d_1", { state: "disconnected", settings_state: "pending", normalization_state: "pending" }, "device.disconnected");
      const result = await saving;
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["not_applied", "not_sent"]);
      expect(keys(fake)).toEqual(["fn.row_default"]);
      expect(fake.find("d_1").settings.find((s) => s.key === "fn.row_default")?.desired).toBe("function_keys");
    } finally { await c.stop(); }
  });

  it.each(["set", "forget"] as const)("sends no queued %s after disconnecting during the idle wait", async (type) => {
    const fake = new FakeAdapter();
    const { c, deps, key } = await opened(fake);
    const publish = deps.published.getMockImplementation()!;
    let waiting = false;
    deps.published.mockImplementation((state) => {
      const published = publish(state);
      if (!waiting && state.devices.find((d) => d.key === key)?.settingsSave?.items[0]?.status === "saving") {
        waiting = true;
        fake.changeDevice("d_1", { settings_state: "applying" });
      }
      return published;
    });
    try {
      const saving = c.act({ type: "settings.save", key, changes: [
        type === "set"
          ? { type: "set", setting: "backlight.enabled", value: false }
          : { type: "forget", setting: "backlight.enabled" },
        { type: "forget", setting: "backlight.delay.hands_out" },
      ] });
      await until(() => waiting);
      fake.changeDevice("d_1", { state: "disconnected", settings_state: "pending", normalization_state: "pending" }, "device.disconnected");
      const result = await saving;
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["not_sent", "not_sent"]);
      expect(writes(fake)).toHaveLength(0);
    } finally { await c.stop(); }
  });

  it("allows offline forgetting while leaving value edits unsent", async () => {
    const fake = new FakeAdapter();
    const { c, key } = await opened(fake);
    try {
      fake.changeDevice("d_1", { state: "disconnected", normalization_state: "pending", settings_state: "pending" });
      const result = await c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "fn.row_default", value: "function_keys" },
        { type: "forget", setting: "backlight.enabled" },
      ] });
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["not_sent", "saved"]);
      expect(writes(fake).map((r) => r.cmd)).toEqual(["hidpp.setting.forget"]);
      expect(fake.find("d_1").settings.find((s) => s.key === "backlight.enabled")).toMatchObject({ managed: false, observed: true });
    } finally { await c.stop(); }
  });

  it("recovers the hardware outcome through resynchronization after a missing row event", async () => {
    const fake = new FakeAdapter({ settingJobMs: 20 });
    const onData = fake.onData.bind(fake);
    fake.onData = (listener) => onData((bytes) => {
      const line = new TextDecoder().decode(bytes).trim();
      try { if (JSON.parse(line).event === "hidpp.setting.changed") return; } catch {}
      listener(bytes);
    });
    const { c, key } = await opened(fake);
    try {
      const result = await c.act({ type: "settings.save", key, changes: [{ type: "set", setting: "fn.row_default", value: "function_keys" }] });
      expect(result.settingsSave?.items[0]?.status).toBe("applied");
      expect(fake.received.filter((r) => r.cmd === "device.list").length).toBeGreaterThan(1);
    } finally { await c.stop(); }
  });

  it("rejects duplicate keys without sending a mutation", async () => {
    const fake = new FakeAdapter();
    const { c, key } = await opened(fake);
    try {
      const changes: SettingsChange[] = [
        { type: "set", setting: "backlight.enabled", value: false },
        { type: "forget", setting: "backlight.enabled" },
      ];
      expect(await c.act({ type: "settings.save", key, changes })).toMatchObject({ ok: false });
      expect(writes(fake)).toHaveLength(0);
    } finally { await c.stop(); }
  });

  it("does not continue a submission on a replacement adapter session", async () => {
    const fake = new FakeAdapter({ settingJobMs: 200 });
    const { c, deps, key } = await opened(fake);
    try {
      const saving = c.act({ type: "settings.save", key, changes: [
        { type: "set", setting: "fn.row_default", value: "function_keys" },
        { type: "set", setting: "backlight.enabled", value: false },
      ] });
      await until(() => fake.find("d_1").device.settings_state === "applying");
      fake.unplug();
      await until(() => c.manager.connected.size === 0);
      const replacement = new FakeAdapter({ adapterId: fake.id });
      deps.ports["/fake"] = replacement;
      await c.manager.rescan();
      const result = await saving;
      expect(result.settingsSave?.items.map((i) => i.status)).toEqual(["not_applied", "not_sent"]);
      expect(writes(replacement)).toHaveLength(0);
    } finally { await c.stop(); }
  });
});
