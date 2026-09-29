import { describe, expect, it, vi } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { requestProblem, responseProblem } from "../src/protocol/validate.ts";
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
    expect(fake.received.slice(0, 5).map((m) => m.cmd)).toEqual([
      "adapter.protocol",
      "adapter.capabilities",
      "adapter.status",
      "session.heartbeat",
      "adapter.wait_ready",
    ]);
    expect(fake.received[0]).toEqual({ v: 0, id: 1, cmd: "adapter.protocol", args: {} });
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

function discoveryTransport(protocol: number) {
  const fake = new FakeAdapter();
  fake.open();
  const close = vi.fn(() => fake.close());
  const transport: Transport = {
    onData(listener) {
      fake.onData((chunk) => {
        const lines = new TextDecoder().decode(chunk).split("\n").filter(Boolean).map((line) => {
          const message = JSON.parse(line);
          if (message.v === 0) message.result = { protocol, future: { values: [1, true, null] } };
          return JSON.stringify(message);
        });
        listener(new TextEncoder().encode(`${lines.join("\n")}\n`));
      });
    },
    onClose: (listener) => fake.onClose(listener),
    write: (text) => fake.write(text),
    close,
  };
  return { fake, close, transport };
}

it("ignores additional discovery result fields and rejects unsupported protocols before management", async () => {
  const hooks = { changed: vi.fn(), closed: vi.fn(), log: vi.fn() };
  const supported = discoveryTransport(1);
  const session = await AdapterSession.open(supported.transport, hooks);
  expect(session.status.protocol).toBe(1);
  await session.close();
  const unsupported = discoveryTransport(2);
  await expect(AdapterSession.open(unsupported.transport, hooks)).rejects.toThrow("unsupported adapter protocol: 2");
  expect(unsupported.fake.received.map((m) => m.cmd)).toEqual(["adapter.protocol"]);
  expect(unsupported.close).toHaveBeenCalled();
});

it("validates the fixed discovery envelope and extensible arguments and result", () => {
  const query = { v: 0, id: 1, cmd: "adapter.protocol" };
  for (const args of [undefined, null, {}, { future: { values: [1, true, null] } }])
    expect(requestProblem("adapter.protocol", args === undefined ? query : { ...query, args })).toBeNull();
  for (const args of [1, false, "", []])
    expect(requestProblem("adapter.protocol", { ...query, args })).not.toBeNull();
  expect(requestProblem("adapter.protocol", { ...query, v: 1 })).not.toBeNull();
  expect(requestProblem("adapter.status", { ...query, cmd: "adapter.status", args: {} })).not.toBeNull();
  const reply = { v: 0, type: "response", id: 1, ok: true, done: true, result: { protocol: 1, future: [1] } };
  expect(responseProblem("adapter.protocol", reply)).toBeNull();
  expect(responseProblem("adapter.protocol", { ...reply, v: 1 })).not.toBeNull();
  expect(responseProblem("adapter.protocol", { ...reply, done: false })).not.toBeNull();
  expect(responseProblem("adapter.protocol", {
    v: 0, type: "response", id: 1, ok: false, done: true, error: { code: "invalid_args" },
  })).not.toBeNull();
  for (const result of [{}, { protocol: "1" }, { protocol: null }, { protocol: -1 }, { protocol: 1.5 }])
    expect(responseProblem("adapter.protocol", { ...reply, result })).not.toBeNull();
});
