import { create } from "@bufbuild/protobuf";
import {
  BacklightMode,
  DeviceSchema,
  ErrorCode,
  ErrorOutcome,
  ErrorReason,
  FunctionRowMode,
  IntegrationKind,
  KeyboardPlatform,
  SettingState,
  SettingsObjectSchema,
  SettingsUpdateSchema,
  StatusSchema,
  VendorRegistry,
  WheelMode,
} from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import * as convert from "../src/core/convert.ts";
import { field } from "./helpers.ts";

const HIDPP = IntegrationKind.HIDPP;

describe("settings objects", () => {
  it("reads every field of every group from its place in the message", () => {
    const objects = [
      create(SettingsObjectSchema, {
        integration: HIDPP,
        group: { case: "keyboard", value: { platform: { value: KeyboardPlatform.MAC, choices: [KeyboardPlatform.MAC] }, functionRow: { saved: FunctionRowMode.FUNCTION_KEYS } } },
      }),
      create(SettingsObjectSchema, { integration: HIDPP, group: { case: "power", value: { autoOff: { value: 0n } } } }),
      create(SettingsObjectSchema, {
        integration: HIDPP,
        group: {
          case: "backlight",
          value: {
            enabled: { value: false },
            mode: { value: BacklightMode.AUTOMATIC },
            brightness: { value: 2n },
            delay: { handsAway: { value: 10n }, handsNearby: { value: 20n }, externalPower: { value: 30n } },
            startupEffect: { value: true },
            dialEffect: { value: false },
            powerSave: { value: true },
            effect: {},
          },
        },
      }),
      create(SettingsObjectSchema, { integration: HIDPP, group: { case: "pointerSensor", value: { index: 1, dpi: { value: 800n } } } }),
      create(SettingsObjectSchema, { integration: HIDPP, group: { case: "wheel", value: { mode: { value: WheelMode.FREE_SPIN }, freeSpinThreshold: { value: 255n }, invert: {} } } }),
      create(SettingsObjectSchema, { integration: HIDPP, group: { case: "thumbwheel", value: { invert: { value: true } } } }),
    ];
    expect(convert.settings(objects).map((s) => [s.group, s.index, s.field, s.type, s.value, s.saved])).toEqual([
      ["keyboard", null, "platform", "enum", "mac", null],
      ["keyboard", null, "function_row", "enum", null, "function_keys"],
      ["power", null, "auto_off", "integer", 0, null],
      ["backlight", null, "enabled", "bool", false, null],
      ["backlight", null, "mode", "enum", "automatic", null],
      ["backlight", null, "brightness", "integer", 2, null],
      ["backlight", null, "hands_away", "integer", 10, null],
      ["backlight", null, "hands_nearby", "integer", 20, null],
      ["backlight", null, "external_power", "integer", 30, null],
      ["backlight", null, "startup_effect", "bool", true, null],
      ["backlight", null, "dial_effect", "bool", false, null],
      ["backlight", null, "power_save", "bool", true, null],
      // A present field without a reading is a capability whose value is unknown.
      ["backlight", null, "effect", "enum", null, null],
      ["pointer_sensor", 1, "dpi", "integer", 800, null],
      ["wheel", null, "mode", "enum", "free_spin", null],
      ["wheel", null, "free_spin_threshold", "integer", 255, null],
      ["wheel", null, "invert", "bool", null, null],
      ["thumbwheel", null, "invert", "bool", true, null],
    ]);
  });

  it("keeps status, limits and only known, distinct choices, and reads uninterpretable enums as unknown", () => {
    const [mode] = convert.settings([
      create(SettingsObjectSchema, {
        integration: HIDPP,
        group: { case: "wheel", value: { mode: { value: 9 as WheelMode, saved: WheelMode.STEPPED, status: { result: { case: "error", value: { code: ErrorCode.TIMEOUT, outcome: ErrorOutcome.UNKNOWN } } }, choices: [WheelMode.STEPPED, WheelMode.STEPPED, WheelMode.UNKNOWN, 9 as WheelMode] } } },
      }),
    ]);
    expect(mode).toMatchObject({ value: "unknown", saved: "stepped", state: null, error: { code: "timeout", reason: "unknown", outcome: "unknown", related: [] }, choices: ["stepped"] });
    const [dpi, other] = convert.settings([
      create(SettingsObjectSchema, {
        integration: HIDPP,
        group: { case: "pointerSensor", value: { index: 0, dpi: { saved: 0n, status: { result: { case: "state", value: SettingState.APPLIED } }, limits: { case: "range", value: { min: 200n, max: 4000n } } } } },
      }),
      create(SettingsObjectSchema, { integration: HIDPP, group: { case: "power", value: { autoOff: { limits: { case: "choices", value: { values: [0n, 60n, 60n] } } } } } }),
    ]);
    expect(dpi).toMatchObject({ index: 0, value: null, saved: 0, state: "applied", min: 200, max: 4000, step: 1 });
    expect(other).toMatchObject({ choices: [0, 60], min: null });
    // A group this app doesn't know has no fields it can show.
    expect(convert.settings([create(SettingsObjectSchema, { integration: HIDPP })])).toEqual([]);
  });

  it("writes each change in its own update at the field's place, keeping false and zero", () => {
    const update = (change: Parameters<typeof convert.settingsUpdate>[0]) => create(SettingsUpdateSchema, convert.settingsUpdate(change));
    const off = update({ type: "set", setting: field("backlight", "enabled"), value: false });
    expect(off.group.case === "backlight" && off.group.value.enabled?.change).toEqual({ case: "value", value: false });
    const zero = update({ type: "set", setting: field("backlight", "external_power"), value: 0 });
    expect(zero.group.case === "backlight" && zero.group.value.delay?.externalPower?.change).toEqual({ case: "value", value: 0n });
    const sensor = update({ type: "set", setting: field("pointer_sensor", "dpi", 0), value: 1600 });
    expect(sensor.group.case === "pointerSensor" && [sensor.group.value.index, sensor.group.value.dpi?.change]).toEqual([0, { case: "value", value: 1600n }]);
    const mode = update({ type: "set", setting: field("wheel", "mode"), value: "stepped" });
    expect(mode.group.case === "wheel" && mode.group.value.mode?.change).toEqual({ case: "value", value: WheelMode.STEPPED });
    const forget = update({ type: "forget", setting: field("keyboard", "function_row") });
    expect(forget.group.case === "keyboard" && forget.group.value.functionRow?.change.case).toBe("forget");
    // The adapter's record of what was sent reads the same changes back.
    for (const u of [off, zero, sensor, mode, forget]) expect(convert.updateChanges(u)).toHaveLength(1);
    expect(convert.updateChanges(zero)).toEqual([{ type: "set", setting: field("backlight", "external_power"), value: 0 }]);
  });
});

describe("information", () => {
  it("keeps zero and false apart from unknown", () => {
    const d = convert.device(create(DeviceSchema, {
      id: 1,
      information: {
        identity: { manufacturer: "", vendor: { registry: VendorRegistry.BLUETOOTH, id: 0 } },
        battery: { level: 0, charging: false },
        backlight: { currentBrightness: 0, delay: { handsAway: 0 } },
        pointerSensors: [{ index: 1, dpi: 800 }, { index: 0 }],
        wheel: { mode: 7 as WheelMode },
      },
    }));
    const i = d.information;
    expect(i.identity).toMatchObject({ manufacturer: "", model: null, vendor: { registry: "bluetooth", id: 0 }, product: { id: null, version: null } });
    expect(i.battery).toEqual({ level: 0, charging: false });
    expect(i.backlight).toMatchObject({ currentBrightness: 0, enabled: null, delay: { handsAway: 0, handsNearby: null, externalPower: null } });
    expect(i.pointerSensors).toEqual([{ index: 0, dpi: null }, { index: 1, dpi: 800 }]);
    expect(i.wheel).toMatchObject({ mode: "unknown", invert: null });
    expect(convert.device(create(DeviceSchema, { id: 1 })).information).toEqual(convert.information(undefined));
  });

  it("reads the adapter's information and resources, keeping unknown counts unknown", () => {
    const s = convert.status(create(StatusSchema, {
      information: { firmwareVersion: "1.0.0", developmentBuild: false },
      resources: [{ type: 1, limit: 16 }],
      profileSupport: { resources: [{ type: 5, count: 0, limit: 0 }] },
    }));
    expect(s.information).toEqual({ firmwareVersion: "1.0.0", boardName: null, developmentBuild: false });
    expect(s.resources).toEqual([{ type: "interfaces", count: null, limit: 16 }]);
    expect(s.profileSupport!.resources).toEqual([{ type: "memory", count: 0, limit: 0 }]);
  });

  it("keeps error details it doesn't know as numbers, so distinct errors stay distinct", () => {
    const s = convert.status(create(StatusSchema, {
      errors: [
        { code: ErrorCode.INTERNAL, reason: 99 as ErrorReason },
        { code: ErrorCode.INTERNAL, reason: 100 as ErrorReason },
        { code: ErrorCode.INTERNAL, reason: 100 as ErrorReason },
        { code: 77 as ErrorCode, outcome: 66 as ErrorOutcome, related: [{ code: ErrorCode.TIMEOUT, reason: 55 as ErrorReason }] },
      ],
    }));
    expect(s.errors).toEqual([
      { code: "internal", reason: 99, outcome: "unspecified", related: [] },
      { code: "internal", reason: 100, outcome: "unspecified", related: [] },
      { code: 77, reason: "unknown", outcome: 66, related: [{ code: "timeout", reason: 55, outcome: "unspecified", related: [] }] },
    ]);
  });
});
