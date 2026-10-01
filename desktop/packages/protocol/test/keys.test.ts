import { expect, it } from "vitest";
import { keys } from "../src/index.ts";

it("matches concrete keys to catalog entries and template indexes", () => {
  expect(keys.lookup(keys.WHEEL_MODE)).toMatchObject({ entry: { key: "wheel.mode", setting: true, values: ["freespin", "ratchet"] }, index: null });
  expect(keys.lookup("pointer.sensor.12.dpi")).toMatchObject({ entry: { key: keys.POINTER_SENSOR_N_DPI, unit: "dpi" }, index: 12 });
  expect(keys.lookup("pointer.sensor.x.dpi")).toBeNull();
  expect(keys.lookup("pointer.sensor..dpi")).toBeNull();
  expect(keys.lookup("wheel.mode.extra")).toBeNull();
  expect(keys.lookup("unknown.key")).toBeNull();
  expect(keys.indexed(keys.POINTER_SENSOR_N_DPI, 1)).toBe("pointer.sensor.1.dpi");
  expect(keys.lookup(keys.STORAGE_FULL)?.entry).toMatchObject({ adapter: true, device: false, type: "bool" });
});
