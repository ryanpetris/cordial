// Ports of simulated adapters, standing in for real ones in every host's
// demo mode.
import type { ByteStream, PortInfo } from "@cordial/client";
import { FakeAdapter, demoCandidatesBle, demoDevices, demoDevicesBle, demoProfiles } from "./adapter.ts";

/** The demo devices with layers using the demo profiles. */
function devicesWithLayers() {
  const devices = demoDevices();
  devices[0]!.profiles = [1];
  devices[1]!.profiles = [2];
  return devices;
}

export function simulatedPorts(count: number) {
  const fakes = new Map<string, FakeAdapter>();
  for (let i = 0; i < count; i++)
    fakes.set(
      `/simulated/${i}`,
      // The ESP32-S3 has no Bluetooth Classic, and has its own sample devices.
      new FakeAdapter(
        i
          ? { adapterId: `00000000000FA0${String(i + 1).padStart(2, "0")}`, board: "xiao_esp32s3", transports: ["ble"], devices: demoDevicesBle(), candidates: demoCandidatesBle(), latency: 5 }
          : { adapterId: "00000000000FA001", board: "pico2_w", transports: ["classic", "ble"], devices: devicesWithLayers(), profiles: demoProfiles(), latency: 5 },
      ),
    );
  return {
    // A simulated adapter leaves the list while its USB reconnects.
    listPorts: async (): Promise<PortInfo[]> => [...fakes].filter(([, f]) => f.present).map(([path, f]) => ({ path, serial: f.serial })),
    openTransport: async (path: string): Promise<ByteStream> => fakes.get(path)!.open(),
    /** Calls `changed` when a simulated adapter's USB comes back after reconnecting. */
    onHotplug: (changed: () => void) => {
      for (const f of fakes.values()) f.onHotplug(changed);
    },
  };
}
