import { expect, it, vi } from "vitest";
import { SerialPort } from "serialport";
import { listPorts, serialMatches } from "../src/node.ts";

vi.mock("serialport", () => ({ SerialPort: { list: vi.fn() } }));

it("discovers Cordial USB IDs regardless of manufacturer and reports each serial number as given", async () => {
  const expected = {
    path: "/dev/ttyACM0",
    vendorId: "1209",
    productId: "C0D1",
    manufacturer: "Cordial",
    serialNumber: "0123456789ABCDEF",
    pnpId: undefined,
    locationId: undefined,
  };
  vi.mocked(SerialPort.list).mockResolvedValue([
    expected,
    { ...expected, path: "/dev/ttyACM6", serialNumber: "0123456789ABCDEF-vial:f64c2b3c" },
    { ...expected, path: "/dev/ttyACM8", serialNumber: undefined },
    ...[
      { vendorId: "1234" },
      { productId: "4001" },
      { manufacturer: undefined },
      { manufacturer: "Other" },
      { manufacturer: "cordial" },
    ].map((fields, i) => ({ ...expected, path: `/dev/ttyACM${i + 1}`, ...fields })),
  ]);
  expect(await listPorts()).toEqual([
    { path: expected.path, serial: expected.serialNumber },
    ...[3, 4, 5].map((i) => ({ path: `/dev/ttyACM${i}`, serial: expected.serialNumber })),
    { path: "/dev/ttyACM6", serial: "0123456789ABCDEF-vial:f64c2b3c" },
    { path: "/dev/ttyACM8", serial: "" },
  ]);
});

it("recognizes an adapter by the first 16 characters of its serial number, without regard to case", () => {
  expect(serialMatches("0123456789ABCDEF", "0123456789ABCDEF")).toBe(true);
  expect(serialMatches("0123456789abcdef-vial:f64c2b3c", "0123456789ABCDEF")).toBe(true);
  expect(serialMatches("0123456789ABCDE", "0123456789ABCDEF")).toBe(false);
  expect(serialMatches("1123456789ABCDEF", "0123456789ABCDEF")).toBe(false);
  expect(serialMatches("", "0123456789ABCDEF")).toBe(false);
});
