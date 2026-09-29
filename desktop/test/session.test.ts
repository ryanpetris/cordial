import { describe, expect, it, vi } from "vitest";
import { AdapterSession } from "../src/core/session.ts";
import type { Transport } from "../src/core/transport.ts";
import { openSession, until } from "./helpers.ts";

describe("AdapterSession", () => {
  it("confirms the adapter, waits for readiness and builds the device view", async () => {
    const { fake, session } = await openSession();
    expect(session.adapterId).toBe(fake.id);
    expect(session.capabilities).toEqual(["classic", "ble"]);
    await until(() => session.view.valid && session.view.infoNeeded().length === 0);
    expect(session.readiness.state).toBe("ready");
    expect([...session.view.devices.keys()]).toEqual(["d_1", "d_2", "d_3", "d_4"]);
    expect(fake.monitor).toBe(true);
    const battery = session.view.info("d_2")!.find((f) => f.key === "battery_percent");
    expect(battery?.value).toBe(12);
    // The handshake order from docs/protocol/transport.md.
    expect(fake.received.slice(0, 4).map((m) => m.cmd)).toEqual([
      "adapter.capabilities",
      "adapter.status",
      "session.heartbeat",
      "adapter.wait_ready",
    ]);
    await session.close();
    expect(fake.monitor).toBe(false);
  });

  it("applies device and information events in revision order", async () => {
    const { fake, session } = await openSession();
    await until(() => session.view.valid && session.view.infoNeeded().length === 0);
    fake.changeInfo("d_2", { battery_percent: 9, battery_charging: true });
    await until(() => session.view.info("d_2")!.find((f) => f.key === "battery_percent")?.value === 9);
    fake.changeDevice("d_3", { state: "connected" }, "device.connected");
    await until(() => session.view.devices.get("d_3")?.state === "connected");
    expect(session.view.valid).toBe(true);
    await session.close();
  });

  it("resynchronizes after lost events or a revision gap", async () => {
    const { fake, session } = await openSession();
    await until(() => session.view.valid && session.view.infoNeeded().length === 0);
    const lists = () => fake.received.filter((m) => m.cmd === "device.list").length;
    fake.loseEvents();
    await until(() => lists() === 2 && session.view.valid);
    fake.revision += 3; // changes whose events never arrive
    fake.changeDevice("d_1", { trusted: false });
    await until(() => lists() === 3 && session.view.valid);
    expect(session.view.devices.get("d_1")?.trusted).toBe(false);
    expect(session.view.revision).toBe(fake.revision);
    await session.close();
  });

  it("reports an unplugged adapter as closed", async () => {
    const { fake, session, hooks } = await openSession();
    await until(() => session.view.valid);
    fake.unplug();
    expect(hooks.closed).toHaveBeenCalledOnce();
    expect(session.closed).toBe(true);
    await expect(session.request("adapter.status", {})).rejects.toThrow("closed");
  });

  it("re-enables monitoring when a heartbeat shows it expired", async () => {
    const { fake, session } = await openSession();
    await until(() => session.view.valid);
    fake.monitor = false; // as after a missed heartbeat deadline
    await until(() => fake.monitor, 7000);
    await until(() => session.view.valid);
    await session.close();
  }, 10000);

  it("rejects a port that doesn't speak the protocol", async () => {
    const closed = vi.fn(async () => {});
    const silent: Transport = {
      onData: () => {},
      onClose: () => {},
      write: async () => {},
      close: closed,
    };
    await expect(AdapterSession.open(silent, { changed: vi.fn(), closed: vi.fn(), log: vi.fn() })).rejects.toThrow(
      "no response",
    );
    expect(closed).toHaveBeenCalled();
  }, 8000);

  it("marks readiness failure without closing", async () => {
    const { session } = await openSession({ readyError: "radio_unavailable" });
    await until(() => session.readiness.state === "failed");
    expect(session.closed).toBe(false);
    await session.close();
  });
});
