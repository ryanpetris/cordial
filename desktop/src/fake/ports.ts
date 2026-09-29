// Ports of simulated adapters, standing in for real ones in every host's
// demo mode.
import type { PortInfo, Transport } from "../core/transport.ts";
import { FakeAdapter } from "./adapter.ts";

export function simulatedPorts(count: number) {
  const fakes = new Map<string, FakeAdapter>();
  for (let i = 0; i < count; i++)
    fakes.set(`/simulated/${i}`, new FakeAdapter({ adapterId: `0000FAKE000${i + 1}`, board: i ? "xiao_esp32s3" : "pico_w", latency: 5 }));
  return {
    listPorts: async (): Promise<PortInfo[]> => [...fakes].map(([path, f]) => ({ path, serial: f.id })),
    openTransport: async (path: string): Promise<Transport> => fakes.get(path)!.open(),
  };
}
