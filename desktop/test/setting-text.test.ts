import { describe, expect, it } from "vitest";
import { displayUnit, neverValue } from "../src/shared/text.ts";

describe("setting value units", () => {
  it("drops the unit beside an automatic power-off of Never", () => {
    expect(neverValue("power.auto_off", 0)).toBe(true);
    expect(displayUnit("power.auto_off", 0)).toBeUndefined();
  });

  it("keeps seconds beside a numeric automatic power-off", () => {
    expect(neverValue("power.auto_off", 600)).toBe(false);
    expect(displayUnit("power.auto_off", 600)).toBe("s");
    expect(displayUnit("power.auto_off", null)).toBe("s");
  });

  it("keeps the unit for zero on other settings", () => {
    expect(neverValue("backlight.delay.powered", 0)).toBe(false);
    expect(displayUnit("backlight.delay.powered", 0)).toBe("s");
  });
});
