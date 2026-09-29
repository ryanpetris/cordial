import { expect, it } from "vitest";
import { adapterName } from "../src/shared/adapter-name.ts";
import { AdapterView } from "../src/core/view.ts";

it("validates canonical names by UTF-8 bytes", () => {
  expect(adapterName("  Desk \u{10400}  ")).toBe("Desk \u{10400}");
  expect(adapterName("é".repeat(32))).toBe("é".repeat(32));
  for (const name of ["", "  ", "é".repeat(33), "x\ny", "x\u0085y", "\ud800"])
    expect(adapterName(name)).toBeNull();
});

it("keeps name and platform together across stale replies and buffered events", () => {
  const view = new AdapterView();
  view.setAdapter(1, "linux", "Pico W");
  view.beginSnapshot();
  view.event({ v: 1, type: "event", event: "adapter.changed", data: { revision: 3, host_platform: "mac", name: "Desk" } });
  view.setAdapter(2, "linux", "Older");
  view.installSnapshot([], 4);
  expect(view.name).toBe("Desk");
  expect(view.platform).toBe("mac");
  view.setAdapter(2, "windows", "Stale response");
  expect(view.name).toBe("Desk");
  expect(view.platform).toBe("mac");
});
