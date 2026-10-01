import { expect, it } from "vitest";
import { adapterName } from "../src/shared/adapter-name.ts";

it("validates canonical names by UTF-8 bytes", () => {
  expect(adapterName("  Desk \u{10400}  ")).toBe("Desk \u{10400}");
  expect(adapterName("é".repeat(32))).toBe("é".repeat(32));
  for (const name of ["", "  ", "é".repeat(33), "x\ny", "x\u0085y", "\ud800"])
    expect(adapterName(name)).toBeNull();
});
