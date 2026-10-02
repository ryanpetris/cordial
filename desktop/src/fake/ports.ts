// Ports of simulated adapters, standing in for real ones in every host's
// demo mode.
import type { ByteStream, PortInfo } from "@cordial/client";
import { FakeAdapter, demoCandidatesBle, demoDevicesBle } from "./adapter.ts";

export function simulatedPorts(count: number) {
  const fakes = new Map<string, FakeAdapter>();
  for (let i = 0; i < count; i++)
    fakes.set(
      `/simulated/${i}`,
      // The ESP32-S3 has no Bluetooth Classic, and has its own sample devices.
      new FakeAdapter(
        i
          ? { adapterId: `0000FAKE000${i + 1}`, board: "xiao_esp32s3", transports: ["ble"], devices: demoDevicesBle(), candidates: demoCandidatesBle(), latency: 5 }
          : { adapterId: `0000FAKE000${i + 1}`, board: "pico_w", transports: ["classic", "ble"], latency: 5 },
      ),
    );
  return {
    listPorts: async (): Promise<PortInfo[]> => [...fakes].map(([path, f]) => ({ path, serial: f.id })),
    openTransport: async (path: string): Promise<ByteStream> => fakes.get(path)!.open(),
  };
}
