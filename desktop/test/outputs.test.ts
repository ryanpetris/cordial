import { create } from "@bufbuild/protobuf";
import { DeviceSchema, ErrorCode, ErrorReason, OutputMode, OutputState, ResourceType, Role, StatusSchema } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { isAction } from "../src/core/actions.ts";
import * as convert from "../src/core/convert.ts";
import { FakeAdapter, device } from "../src/fake/adapter.ts";
import { trayModel } from "../src/main/tray-model.ts";
import { resource } from "../src/shared/capacity.ts";
import { gamepadModes, gamepadOutput, offersGamepadOutput } from "../src/shared/outputs.ts";
import type { AdapterStatus, AppState, Code } from "../src/shared/state.ts";
import { codeText } from "../src/shared/text.ts";
import { controller, record, until } from "./helpers.ts";

const ID = "AAAA0001";

const support = (modes: AdapterStatus["outputSupport"][number]["modes"]): AdapterStatus => ({
  id: ID,
  name: "A",
  platform: "linux",
  ready: true,
  errors: [],
  resources: [],
  transports: [],
  information: { firmwareVersion: null, boardName: null, developmentBuild: null },
  profileSupport: null,
  interfaces: [],
  outputSupport: [{ role: "gamepad", modes }],
});
const slots = (count: number | null, limit: number | null) => [{ type: "slots" as const, count, limit }];
const BOTH = support([
  { mode: "disabled", resources: slots(0, 0) },
  { mode: "hid", resources: slots(1, 4) },
  { mode: "xinput", resources: slots(0, 4) },
]);
const NONE: AdapterStatus = { ...BOTH, outputSupport: [] };

const pad = (id: number) => record(id, { kinds: ["gamepad"], roles: ["gamepad"] });

describe("Output conversion", () => {
  it("keeps known output support and skips roles and modes this app doesn't know", () => {
    const s = convert.status(create(StatusSchema, {
      outputSupport: [
        {
          role: Role.GAMEPAD,
          modes: [
            { mode: OutputMode.DISABLED, resources: [{ type: ResourceType.SLOTS, count: 0, limit: 0 }] },
            // A count not yet known stays unknown; an unknown type and a repeated one are skipped.
            { mode: OutputMode.XINPUT, resources: [{ type: 42 as ResourceType, count: 1 }, { type: ResourceType.SLOTS, limit: 4 }, { type: ResourceType.SLOTS, count: 3, limit: 4 }] },
            { mode: 9 as OutputMode, resources: [{ type: ResourceType.SLOTS, limit: 4 }] },
          ],
        },
        { role: 42 as Role, modes: [{ mode: OutputMode.HID }] },
      ],
    }));
    expect(s.outputSupport).toEqual([{ role: "gamepad", modes: [{ mode: "disabled", resources: slots(0, 0) }, { mode: "xinput", resources: slots(null, 4) }] }]);
  });

  it("reads output preferences and states, with unknown modes as null and unknown or unset states as unknown", () => {
    const d = convert.device(create(DeviceSchema, {
      id: 1,
      outputPreferences: [{ role: Role.GAMEPAD, mode: 7 as OutputMode }, { role: 42 as Role, mode: OutputMode.HID }],
      outputs: [
        { role: Role.GAMEPAD, mode: OutputMode.HID, status: { case: "state", value: OutputState.ACTIVE }, slot: 0 },
        { role: Role.GAMEPAD, mode: 7 as OutputMode, status: { case: "state", value: 99 as OutputState } },
        { role: Role.GAMEPAD, mode: OutputMode.XINPUT },
        { role: Role.GAMEPAD, mode: OutputMode.HID, status: { case: "error", value: { code: ErrorCode.UNSUPPORTED, reason: ErrorReason.INPUT_LAYOUT } } },
        { role: Role.UNKNOWN, mode: OutputMode.HID, status: { case: "state", value: OutputState.PENDING } },
      ],
    }));
    expect(d.outputPreferences).toEqual([{ role: "gamepad", mode: null }]);
    expect(d.outputs).toEqual([
      { role: "gamepad", mode: "hid", state: "active", error: null, slot: 0 },
      { role: "gamepad", mode: null, state: "unknown", error: null, slot: null },
      { role: "gamepad", mode: "xinput", state: "unknown", error: null, slot: null },
      { role: "gamepad", mode: "hid", state: null, error: { code: "unsupported", reason: "input_layout", outcome: "unspecified", related: [] }, slot: null },
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
    expect(gamepadModes(support([{ mode: "xinput", resources: [] }]))).toEqual(["disabled", "xinput"]);
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

/** Every adapter is ready and `devices` devices are listed. */
const loaded = (s: AppState | null, devices: number) =>
  !!s && s.adapters.length > 0 && s.adapters.every((a) => a.readiness === "ready") && s.devices.length === devices && s.devices.every((d) => d.settings !== null);

describe("Gamepad Output changes", () => {
  const controllers = () => [
    device(1, { name: "Pad One", kinds: ["gamepad"], roles: ["gamepad"], state: "connected" }),
    device(2, { name: "Pad Two", kinds: ["gamepad"], roles: ["gamepad"], gamepadOutput: "hid" }),
    device(3, { name: "Keys" }),
  ];

  it("saves a mode, follows the reported state, and reconnects USB when the allocation changes", async () => {
    // Serial and three gamepad slots fit: Pad Two's configured HID slot first, then unconfigured ones.
    const fake = new FakeAdapter({ adapterId: ID, devices: controllers(), usbInterfaces: 5 });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const entry = (id: number) => state()!.devices.find((d) => d.key === `${ID}/${id}`)!.device;
    const adapter = () => state()!.adapters[0]!;
    const pools = () => adapter().status!.outputSupport[0]!.modes.map((m) => [m.mode, resource(m.resources, "slots")?.count]);
    expect(pools()).toEqual([["disabled", 0], ["hid", 3], ["xinput", 0]]);
    expect(entry(1).outputs).toEqual([{ role: "gamepad", mode: "disabled", state: "disabled", error: null, slot: null }]);
    expect(entry(2).outputs).toEqual([{ role: "gamepad", mode: "hid", state: "disconnected", error: null, slot: null }]);
    expect(entry(3).outputs).toEqual([]);

    // A connected XInput controller takes a slot ahead of the unconfigured HID ones.
    expect(await c.act({ type: "device.update", key: `${ID}/1`, gamepadOutput: "xinput" })).toEqual({ ok: true });
    expect(fake.received.filter((r) => r.command.case === "setDevice").at(-1)!.command.value).toMatchObject({
      device: 1,
      outputPreferences: [{ role: Role.GAMEPAD, mode: OutputMode.XINPUT }],
      integrations: [],
    });
    await until(() => adapter().connection === "reconnecting");
    expect(fake.usbReconnects).toBe(1);
    await until(() => fake.present);
    await c.manager.rescan();
    await until(() => adapter().connection === "connected" && loaded(state(), 3));
    expect(entry(1).outputPreferences).toEqual([{ role: "gamepad", mode: "xinput" }]);
    expect(entry(1).outputs).toEqual([{ role: "gamepad", mode: "xinput", state: "active", error: null, slot: 0 }]);
    expect(pools()).toEqual([["disabled", 0], ["hid", 2], ["xinput", 1]]);
    expect(adapter().status!.errors).toEqual([]);

    // Disconnecting releases the slot without reconnecting USB.
    expect(await c.act({ type: "device.disconnect", key: `${ID}/1` })).toEqual({ ok: true });
    await until(() => entry(1).outputs[0]!.state === "disconnected");
    expect(entry(1).outputs[0]!.slot).toBeNull();
    expect(fake.usbReconnects).toBe(1);
    await c.stop();
  });

  it("reports a save whose answer USB cut off as unknown, and shows what was saved once reconnected", async () => {
    const fake = new FakeAdapter({ adapterId: ID, devices: controllers(), usbInterfaces: 5 });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const release = fake.hold();
    const pending = c.act({ type: "device.update", key: `${ID}/1`, gamepadOutput: "xinput" });
    await until(() => fake.received.some((r) => r.command.case === "setDevice"));
    // The adapter detaches before the response leaves it.
    fake.unplug();
    release();
    expect(await pending).toEqual({ ok: false, message: "The adapter couldn't confirm whether the change was saved." });
    // USB still reconnects for the new allocation.
    await until(() => state()!.adapters[0]!.connection === "reconnecting" && fake.usbReconnects === 1);
    await until(() => fake.present);
    await c.manager.rescan();
    await until(() => state()!.adapters[0]!.connection === "connected" && loaded(state(), 3));
    expect(state()!.devices.find((d) => d.key === `${ID}/1`)!.device.outputPreferences).toEqual([{ role: "gamepad", mode: "xinput" }]);
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
    const fake = new FakeAdapter({ adapterId: ID, board: "esp32s3_8mb", transports: ["ble"], devices: controllers() });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 3));
    const entry = (id: number) => state()!.devices.find((d) => d.key === `${ID}/${id}`)!.device;
    expect(state()!.adapters[0]!.status!.outputSupport).toEqual([]);
    expect(entry(2).outputs).toEqual([{ role: "gamepad", mode: "hid", state: null, error: { code: "unsupported", reason: "output_backend", outcome: "unspecified", related: [] }, slot: null }]);
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

  it("reports omitted required and reserved interfaces independently, and unconfigured ones not at all", async () => {
    const devices = [
      device(1, { name: "Live", kinds: ["gamepad"], roles: ["gamepad"], state: "connected", gamepadOutput: "hid" }),
      device(2, { name: "Second", kinds: ["gamepad"], roles: ["gamepad"], gamepadOutput: "hid" }),
      device(3, { name: "Other", kinds: ["gamepad"], roles: ["gamepad"], gamepadOutput: "xinput" }),
    ];
    // Serial takes two interfaces, the connected HID controller one, the first XInput slot one and
    // the second HID slot one.
    const cases: [number, Code[]][] = [
      [3, ["usb_required_capacity_exceeded", "usb_reserved_capacity_exceeded"]],
      [4, ["usb_reserved_capacity_exceeded"]],
      [5, []],
    ];
    for (const [usbInterfaces, errors] of cases) {
      const fake = new FakeAdapter({ adapterId: ID, devices: structuredClone(devices), usbInterfaces });
      const { c, state } = controller({ "/a": fake });
      await c.manager.rescan();
      await until(() => loaded(state(), 3));
      const adapter = state()!.adapters[0]!;
      expect(adapter.status!.errors).toEqual(errors.map((code) => ({ code, reason: "output_backend", outcome: "unspecified", related: [] })));
      expect(resource(adapter.status!.resources, "interfaces")).toEqual({ type: "interfaces", count: usbInterfaces, limit: usbInterfaces });
      // Only the required capacity needs attention, on the adapter and in the tray.
      const required = errors.includes("usb_required_capacity_exceeded");
      expect(adapter.attention).toEqual(required ? [codeText("usb_required_capacity_exceeded")] : []);
      expect(trayModel(state()!).badge).toBe(required ? "attention" : "none");
      await c.stop();
    }
  });
});
