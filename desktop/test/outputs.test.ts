import { create } from "@bufbuild/protobuf";
import { DeviceSchema, ErrorCode, OutputMode, OutputState, Role, StatusSchema } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { isAction } from "../src/core/actions.ts";
import * as convert from "../src/core/convert.ts";
import { FakeAdapter, device } from "../src/fake/adapter.ts";
import { gamepadModes, gamepadOutput, offersGamepadOutput, reconnectsGamepadUsb } from "../src/shared/outputs.ts";
import type { AdapterStatus, AppState, DeviceRecord } from "../src/shared/state.ts";
import { codeText } from "../src/shared/text.ts";
import { controller, record, until } from "./helpers.ts";

const ID = "AAAA0001";

const support = (modes: AdapterStatus["outputSupport"][number]["modes"]): AdapterStatus => ({
  id: ID,
  name: "A",
  platform: "linux",
  ready: true,
  transports: [],
  info: [],
  profileSupport: null,
  interfaces: [],
  outputSupport: [{ role: "gamepad", modes }],
});
const BOTH = support([
  { mode: "disabled", maxSlots: 0, slots: null },
  { mode: "hid", maxSlots: 4, slots: 1 },
  { mode: "xinput", maxSlots: 4, slots: 0 },
]);
const NONE: AdapterStatus = { ...BOTH, outputSupport: [] };

const pad = (id: number) => record(id, { kinds: ["gamepad"], roles: ["gamepad"] });

describe("Output conversion", () => {
  it("keeps known output support and skips roles and modes this app doesn't know", () => {
    const s = convert.status(create(StatusSchema, {
      outputSupport: [
        { role: Role.GAMEPAD, modes: [{ mode: OutputMode.DISABLED }, { mode: OutputMode.XINPUT, maxSlots: 4, slots: 2 }, { mode: 9 as OutputMode, maxSlots: 4 }] },
        { role: 42 as Role, modes: [{ mode: OutputMode.HID, maxSlots: 4 }] },
      ],
    }));
    expect(s.outputSupport).toEqual([{ role: "gamepad", modes: [{ mode: "disabled", maxSlots: 0, slots: null }, { mode: "xinput", maxSlots: 4, slots: 2 }] }]);
  });

  it("reads output preferences and states, with unknown modes as null and unknown states as unknown", () => {
    const d = convert.device(create(DeviceSchema, {
      id: 1,
      outputPreferences: [{ role: Role.GAMEPAD, mode: 7 as OutputMode }, { role: 42 as Role, mode: OutputMode.HID }],
      outputs: [
        { role: Role.GAMEPAD, mode: OutputMode.HID, state: OutputState.ACTIVE, slot: 0 },
        { role: Role.GAMEPAD, mode: 7 as OutputMode, state: 99 as OutputState },
        { role: Role.UNKNOWN, mode: OutputMode.HID, state: OutputState.PENDING },
      ],
    }));
    expect(d.outputPreferences).toEqual([{ role: "gamepad", mode: null }]);
    expect(d.outputs).toEqual([
      { role: "gamepad", mode: "hid", state: "active", slot: 0 },
      { role: "gamepad", mode: null, state: "unknown", slot: null },
    ]);
    expect(gamepadOutput(d)).toBeNull();
    expect(gamepadOutput(record(2))).toBe("disabled");
  });

  it("accepts only known Gamepad Output modes in actions", () => {
    expect(isAction({ type: "device.update", key: "a/1", gamepadOutput: "xinput" })).toBe(true);
    expect(isAction({ type: "device.update", key: "a/1", gamepadOutput: "disabled" })).toBe(true);
    expect(isAction({ type: "device.update", key: "a/1", gamepadOutput: "dinput" })).toBe(false);
    expect(isAction({ type: "device.update", key: "a/1", gamepadOutput: 1 })).toBe(false);
  });
});

describe("Gamepad Output choices", () => {
  it("offers the advertised modes after Disabled, and none without support", () => {
    expect(gamepadModes(BOTH)).toEqual(["disabled", "hid", "xinput"]);
    expect(gamepadModes(support([{ mode: "xinput", maxSlots: 4, slots: null }]))).toEqual(["disabled", "xinput"]);
    expect(gamepadModes(NONE)).toEqual(["disabled"]);
    expect(gamepadModes(null)).toEqual(["disabled"]);
  });

  it("shows on gamepads with support, and on any device with a saved choice", () => {
    expect(offersGamepadOutput(pad(1), BOTH)).toBe(true);
    expect(offersGamepadOutput(record(1, { roles: ["gamepad"], kinds: [] }), BOTH)).toBe(true);
    expect(offersGamepadOutput(pad(1), NONE)).toBe(false);
    expect(offersGamepadOutput(record(1), BOTH)).toBe(false);
    // A saved choice can be turned off, whatever the adapter or the device's roles.
    expect(offersGamepadOutput(record(1, { outputPreferences: [{ role: "gamepad", mode: "hid" }] }), NONE)).toBe(true);
    expect(offersGamepadOutput(record(1, { outputPreferences: [{ role: "gamepad", mode: null }] }), NONE)).toBe(true);
  });
});

describe("Gamepad Output pools", () => {
  const choosing = (id: number, mode: "hid" | "xinput" | null, patch: Partial<DeviceRecord> = {}) =>
    record(id, { outputPreferences: [{ role: "gamepad", mode }], ...patch });

  it("reconnects USB when either pool's size changes", () => {
    const one = pad(1);
    expect(reconnectsGamepadUsb([one], one, "hid", BOTH)).toBe(true);
    expect(reconnectsGamepadUsb([one], one, "xinput", BOTH)).toBe(true);
    const hid = choosing(1, "hid");
    expect(reconnectsGamepadUsb([hid], hid, "disabled", BOTH)).toBe(true);
    expect(reconnectsGamepadUsb([hid], hid, "xinput", BOTH)).toBe(true);
    // The device itself counts even when the list doesn't include it.
    expect(reconnectsGamepadUsb([], hid, "disabled", BOTH)).toBe(true);
  });

  it("keeps USB connected when the pools keep their sizes", () => {
    const one = pad(1);
    expect(reconnectsGamepadUsb([one], one, "disabled", BOTH)).toBe(false);
    const hid = choosing(1, "hid");
    expect(reconnectsGamepadUsb([hid], hid, "hid", BOTH)).toBe(false);
  });

  it("counts every paired device choosing a mode, connected or not, up to four", () => {
    const others = [
      choosing(2, "hid", { state: "connected" }),
      choosing(3, "hid", { enabled: false }),
      choosing(4, "hid", { blocked: true, inactive: "transport_disabled" }),
      choosing(5, "hid", { inactive: "capacity" }),
    ];
    const one = pad(1);
    // HID is full with four other devices, so a fifth leaves its size alone, but leaving
    // XInput for it still shrinks XInput.
    expect(reconnectsGamepadUsb([one, ...others], one, "hid", BOTH)).toBe(false);
    const x = choosing(1, "xinput");
    expect(reconnectsGamepadUsb([x, ...others], x, "hid", BOTH)).toBe(true);
    // With three others, a fourth grows HID.
    expect(reconnectsGamepadUsb([one, ...others.slice(1)], one, "hid", BOTH)).toBe(true);
    // Both pools full before and after.
    const xs = [6, 7, 8, 9].map((id) => choosing(id, "xinput"));
    const five = choosing(1, "xinput");
    expect(reconnectsGamepadUsb([five, ...others, ...xs], five, "hid", BOTH)).toBe(false);
  });

  it("counts a mode this app doesn't know in neither pool", () => {
    const unknown = choosing(1, null);
    expect(reconnectsGamepadUsb([unknown], unknown, "disabled", BOTH)).toBe(false);
    expect(reconnectsGamepadUsb([unknown, choosing(2, null)], unknown, "hid", BOTH)).toBe(true);
  });

  it("counts only the modes the adapter offers", () => {
    // Clearing a choice the adapter can't use changes no pool.
    const hid = choosing(1, "hid");
    const xinput = choosing(2, "xinput");
    expect(reconnectsGamepadUsb([hid, xinput], hid, "disabled", NONE)).toBe(false);
    expect(reconnectsGamepadUsb([hid, xinput], xinput, "disabled", null)).toBe(false);
    const xOnly = support([{ mode: "disabled", maxSlots: 0, slots: null }, { mode: "xinput", maxSlots: 4, slots: 1 }]);
    expect(reconnectsGamepadUsb([hid, xinput], hid, "disabled", xOnly)).toBe(false);
    // An offered pool still changes size.
    expect(reconnectsGamepadUsb([hid, xinput], xinput, "disabled", xOnly)).toBe(true);
    expect(reconnectsGamepadUsb([hid, xinput], hid, "xinput", xOnly)).toBe(true);
  });
});

/** Every adapter is ready and `devices` devices are listed. */
const loaded = (s: AppState | null, devices: number) =>
  !!s && s.adapters.length > 0 && s.adapters.every((a) => a.readiness === "ready") && s.devices.length === devices && s.devices.every((d) => d.settings !== null);

describe("Gamepad Output changes", () => {
  const controllers = () => [
    device(1, { name: "Pad One", kinds: ["gamepad"], roles: ["gamepad"], state: "connected" }),
    device(2, { name: "Pad Two", kinds: ["gamepad"], roles: ["gamepad"], gamepadOutput: "hid" }),
    device(3, { name: "Keys" }),
  ];

  it("saves a mode, follows the reported state, and reconnects USB when a pool's size changes", async () => {
    const fake = new FakeAdapter({ adapterId: ID, devices: controllers() });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const entry = (id: number) => state()!.devices.find((d) => d.key === `${ID}/${id}`)!.device;
    const adapter = () => state()!.adapters[0]!;
    expect(adapter().status!.outputSupport[0]!.modes.map((m) => [m.mode, m.slots])).toEqual([["disabled", null], ["hid", 1], ["xinput", 0]]);
    expect(entry(1).outputs).toEqual([{ role: "gamepad", mode: "disabled", state: "disabled", slot: null }]);
    expect(entry(2).outputs).toEqual([{ role: "gamepad", mode: "hid", state: "disconnected", slot: null }]);
    expect(entry(3).outputs).toEqual([]);

    // A second HID controller grows the HID pool from one slot to two.
    expect(await c.act({ type: "device.update", key: `${ID}/1`, gamepadOutput: "hid" })).toEqual({ ok: true });
    expect(fake.received.filter((r) => r.command.case === "setDevice").at(-1)!.command.value).toMatchObject({
      device: 1,
      outputPreferences: [{ role: Role.GAMEPAD, mode: OutputMode.HID }],
      integrations: [],
    });
    await until(() => adapter().connection === "connecting");
    expect(fake.usbReconnects).toBe(1);
    await until(() => fake.present);
    await c.manager.rescan();
    await until(() => adapter().connection === "connected" && loaded(state(), 3));
    expect(entry(1).outputPreferences).toEqual([{ role: "gamepad", mode: "hid" }]);
    expect(entry(1).outputs).toEqual([{ role: "gamepad", mode: "hid", state: "active", slot: 0 }]);
    expect(adapter().status!.outputSupport[0]!.modes.find((m) => m.mode === "hid")!.slots).toBe(2);

    // Disconnecting releases the slot without reconnecting USB.
    expect(await c.act({ type: "device.disconnect", key: `${ID}/1` })).toEqual({ ok: true });
    await until(() => entry(1).outputs[0]!.state === "disconnected");
    expect(entry(1).outputs[0]!.slot).toBeNull();
    expect(fake.usbReconnects).toBe(1);
    await c.stop();
  });

  it("keeps USB connected when the pools keep their sizes", async () => {
    const devices = [...controllers(), ...[4, 5, 6, 7].map((id) => device(id, { kinds: ["gamepad"], roles: ["gamepad"], gamepadOutput: "xinput" }))];
    const fake = new FakeAdapter({ adapterId: ID, devices });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 7));
    const entry = (id: number) => state()!.devices.find((d) => d.key === `${ID}/${id}`)!.device;
    // A fifth XInput choice leaves the XInput pool at four.
    expect(await c.act({ type: "device.update", key: `${ID}/3`, gamepadOutput: "xinput" })).toEqual({ ok: true });
    await until(() => entry(3).outputPreferences.length === 1);
    expect(entry(3).outputPreferences).toEqual([{ role: "gamepad", mode: "xinput" }]);
    expect(fake.received.filter((r) => r.command.case === "getDevice")).toEqual([]);
    expect(fake.usbReconnects).toBe(0);
    expect(state()!.adapters[0]!.connection).toBe("connected");
    await c.stop();
  });

  it("refuses enabled modes the adapter doesn't advertise, and turns a stale choice off", async () => {
    const fake = new FakeAdapter({ adapterId: ID, board: "xiao_esp32s3", transports: ["ble"], devices: controllers() });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const entry = (id: number) => state()!.devices.find((d) => d.key === `${ID}/${id}`)!.device;
    expect(state()!.adapters[0]!.status!.outputSupport).toEqual([]);
    expect(entry(2).outputs).toEqual([{ role: "gamepad", mode: "hid", state: "unsupported", slot: null }]);
    const before = fake.received.length;
    expect(await c.act({ type: "device.update", key: `${ID}/1`, gamepadOutput: "xinput" })).toEqual({ ok: false, message: codeText("unsupported") });
    expect(fake.received.length).toBe(before);
    expect(await c.act({ type: "device.update", key: `${ID}/2`, gamepadOutput: "disabled" })).toEqual({ ok: true });
    await until(() => entry(2).outputs[0]?.state === "disabled");
    expect(entry(2).outputPreferences).toEqual([]);
    expect(fake.usbReconnects).toBe(0);
    // The adapter's own refusal is shown as it is.
    fake.failures.setDevice = [ErrorCode.STORAGE_FAILED];
    expect(await c.act({ type: "device.update", key: `${ID}/1`, gamepadOutput: "disabled" })).toMatchObject({ ok: false });
    await c.stop();
  });
});
