import { expect, it, vi } from "vitest";
import { SerialPort } from "serialport";
import { candidatePorts } from "../src/node/serial.ts";

vi.mock("serialport", () => ({ SerialPort: { list: vi.fn() } }));

it("discovers only ports with the Cordial USB ID and manufacturer", async () => {
  const expected = {
    path: "/dev/ttyACM0",
    vendorId: "CAFE",
    productId: "4014",
    manufacturer: "Cordial",
    serialNumber: "0123456789ABCDEF",
    pnpId: undefined,
    locationId: undefined,
  };
  vi.mocked(SerialPort.list).mockResolvedValue([
    expected,
    { ...expected, path: "/dev/ttyACM8", serialNumber: undefined },
    ...[
      { vendorId: "1234" },
      { productId: "4001" },
      { manufacturer: undefined },
      { manufacturer: "Other" },
      { manufacturer: "cordial" },
    ].map((fields, i) => ({ ...expected, path: `/dev/ttyACM${i + 1}`, ...fields })),
  ]);
  expect(await candidatePorts()).toEqual([
    { path: expected.path, serial: expected.serialNumber },
    { path: "/dev/ttyACM8", serial: "" },
  ]);
});
