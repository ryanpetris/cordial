// Revision 1 is the first, so there is no older adapter to convert for yet. These tests build the
// app as if it were on revision 2 to exercise the Cordial CLI handoff.
import type { ByteStream } from "@cordial/client";
import { CURRENT_PROTOCOL_VERSION, type AdapterHello } from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { HelperError } from "../src/core/manager.ts";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { MIGRATION_TEXT, REFUSALS } from "../src/shared/text.ts";
import { controller, until } from "./helpers.ts";

vi.mock("@cordial/protocol", async (original) => ({ ...(await original<typeof import("@cordial/protocol")>()), CURRENT_PROTOCOL_VERSION: 2 }));

const EXIT_MS = 30;

/** A simulated Cordial CLI: the adapter opened at the current revision. Its stream reports an end
 * and settles a close only once the simulated process has exited. */
function cli(fake: FakeAdapter, events: string[]) {
  let alive = 0;
  const started: { native: AdapterHello; die(): void }[] = [];
  const openProxy = vi.fn(async (_path: string, native: AdapterHello, signal: AbortSignal): Promise<{ stream: ByteStream; hello: AdapterHello }> => {
    if (signal.aborted) throw new Error("opening cancelled");
    events.push("cli start");
    alive++;
    const inner = fake.open(CURRENT_PROTOCOL_VERSION);
    let exited: Promise<void> | null = null;
    const exit = () => (exited ??= new Promise<void>((resolve) => setTimeout(() => {
      alive--;
      events.push("cli exit");
      resolve();
    }, EXIT_MS)));
    started.push({ native, die: () => fake.unplug() });
    const stream: ByteStream = {
      onData: (listener) => inner.onData(listener),
      onClose: (listener) => inner.onClose((error) => void exit().then(() => listener(error))),
      write: (bytes) => inner.write(bytes),
      close: async () => {
        await inner.close();
        await exit();
      },
    };
    return { stream, hello: native };
  });
  return { openProxy, started, alive: () => alive };
}

async function start(options: { native?: number } = {}) {
  const fake = new FakeAdapter({ adapterId: "AAAA0001" });
  fake.protocolVersion = options.native ?? 1;
  const events: string[] = [];
  const helper = cli(fake, events);
  const harness = controller({ "/a": fake }, { openProxy: helper.openProxy });
  harness.ports.open = async () => {
    // Nothing may open the port while a CLI still holds it.
    if (helper.alive()) throw new Error("the port is still held by the Cordial CLI");
    events.push("direct open");
    const stream = fake.open();
    return {
      onData: (listener) => stream.onData(listener),
      onClose: (listener) => stream.onClose(listener),
      write: (bytes) => stream.write(bytes),
      close: async () => {
        await stream.close();
        events.push("direct close");
      },
    } satisfies ByteStream;
  };
  return { fake, events, helper, ...harness };
}

describe("an adapter on an older protocol", () => {
  it("opens through the Cordial CLI once the direct session has closed", async () => {
    const { fake, events, helper, c } = await start();
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      expect(events).toEqual(["direct open", "direct close", "cli start"]);
      expect(helper.openProxy).toHaveBeenCalledOnce();
      expect(helper.openProxy.mock.calls[0]![0]).toBe("/a");
      expect(helper.started[0]!.native).toMatchObject({ protocolVersion: 1, serialNumber: fake.id, firmwareVersion: "0.0.0-dev" });
      expect(fake.hellos).toBe(1);
      expect(c.state().adapters).toMatchObject([{ id: fake.id, connection: "connected", refused: null }]);
    } finally {
      await c.stop();
    }
  });

  it("treats the CLI's exit as a disconnect and recovers through a fresh CLI after it exits", async () => {
    const { fake, events, helper, c } = await start();
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      const entry = c.manager.connected.get(fake.id)!;
      helper.started[0]!.die();
      await until(() => c.state().adapters[0]!.connection === "reconnecting");
      await until(() => helper.openProxy.mock.calls.length === 2 && entry.session.available);
      expect(c.manager.connected.get(fake.id)).toBe(entry);
      expect(events).toEqual(["direct open", "direct close", "cli start", "cli exit", "direct open", "direct close", "cli start"]);
      expect(helper.alive()).toBe(1);
    } finally {
      await c.stop();
    }
    expect(helper.alive()).toBe(0);
  });

  it("doesn't replay a request the CLI's exit interrupted", async () => {
    const { fake, helper, c } = await start();
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      const release = fake.hold();
      const renaming = c.act({ type: "adapter.name", adapterId: fake.id, name: "Desk" });
      await until(() => fake.received.some((r) => r.command.case === "setAdapter"));
      helper.started[0]!.die();
      release();
      expect(await renaming).toEqual({ ok: false, message: "The adapter couldn't confirm whether the change was saved." });
      await until(() => helper.openProxy.mock.calls.length === 2 && c.manager.connected.get(fake.id)!.session.available);
      expect(fake.received.some((r) => r.command.case === "setAdapter")).toBe(false);
    } finally {
      await c.stop();
    }
  });

  it("connects directly without the CLI once the adapter returns on the current protocol", async () => {
    const { fake, events, helper, c } = await start();
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      fake.protocolVersion = CURRENT_PROTOCOL_VERSION;
      helper.started[0]!.die();
      await until(() => events.at(-1) === "direct open" && c.manager.connected.get(fake.id)!.session.available);
      expect(helper.openProxy).toHaveBeenCalledOnce();
      expect(helper.alive()).toBe(0);
    } finally {
      await c.stop();
    }
  });

  it("refuses the adapter when it returns on a newer protocol", async () => {
    const { fake, helper, c } = await start();
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      fake.protocolVersion = CURRENT_PROTOCOL_VERSION + 1;
      helper.started[0]!.die();
      await until(() => c.manager.refused.size === 1);
      expect(c.state().adapters).toMatchObject([{ id: "port:/a", connectError: REFUSALS.newer }]);
      expect(helper.openProxy).toHaveBeenCalledOnce();
    } finally {
      await c.stop();
    }
  });

  it.each([
    ["missing", REFUSALS.missing],
    ["incompatible", REFUSALS.incompatible],
    ["unsupported", REFUSALS.unsupported],
  ] as const)("is refused when the CLI is %s", async (kind, message) => {
    const fake = new FakeAdapter();
    fake.protocolVersion = 1;
    const openProxy = vi.fn(async () => {
      throw new HelperError(kind, "details");
    });
    const { c } = controller({ "/a": fake }, { openProxy });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      expect(c.state().adapters).toMatchObject([{ id: "port:/a", name: fake.name, connectError: message, refused: { serialNumber: fake.id } }]);
      expect(fake.received).toEqual([]);
      await c.manager.rescan();
      expect(openProxy).toHaveBeenCalledOnce();
    } finally {
      await c.stop();
    }
  });

  it("is refused where the CLI can't run", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = 1;
    const { c } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      expect(c.state().adapters).toMatchObject([{ connectError: REFUSALS.browser }]);
    } finally {
      await c.stop();
    }
  });

  it("doesn't keep a session through the CLI with another adapter", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    fake.protocolVersion = 1;
    const other = new FakeAdapter({ adapterId: "BBBB0002" });
    const { c, deps } = controller({ "/a": fake }, { openProxy: async (_path, native) => ({ stream: other.open(), hello: native }) });
    try {
      await c.manager.rescan();
      await until(() => deps.log.mock.calls.some(([m]) => m === "/a: another adapter answered through the Cordial CLI"));
      expect(c.manager.connected.size).toBe(0);
      expect(c.manager.refused.size).toBe(0);
    } finally {
      await c.stop();
    }
  });

  it("waits for a starting CLI to exit when stopping", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = 1;
    let exited = false;
    const openProxy = vi.fn((_path: string, _native: AdapterHello, signal: AbortSignal) => new Promise<never>((_resolve, reject) => {
      signal.addEventListener("abort", () => setTimeout(() => {
        exited = true;
        reject(new Error("opening cancelled"));
      }, EXIT_MS));
    }));
    const { c } = controller({ "/a": fake }, { openProxy });
    await c.manager.rescan();
    await until(() => openProxy.mock.calls.length === 1);
    await c.stop();
    expect(exited).toBe(true);
    expect(c.manager.refused.size).toBe(0);
  });

  it("waits for a CLI that became ready as stopping cancelled it to exit", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = 1;
    const events: string[] = [];
    const helper = cli(fake, events);
    let ready = () => {};
    const openProxy = vi.fn((path: string, native: AdapterHello, signal: AbortSignal) =>
      new Promise<{ stream: ByteStream; hello: AdapterHello }>((resolve) => (ready = () => resolve(helper.openProxy(path, native, signal)))));
    const { c } = controller({ "/a": fake }, { openProxy });
    await c.manager.rescan();
    await until(() => openProxy.mock.calls.length === 1);
    ready();
    const stopping = c.stop();
    expect(helper.alive()).toBe(1);
    await stopping;
    expect(helper.alive()).toBe(0);
    expect(fake.hellos).toBe(0);
  });

  it("connects through the CLI when no hello names a serial number", async () => {
    const { fake, helper, c } = await start();
    fake.helloMetadata = false;
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      expect(helper.started[0]!.native.serialNumber).toBeUndefined();
    } finally {
      await c.stop();
    }
  });

  it("doesn't keep a session when the CLI's hello from the adapter names another serial number than the converted one", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    fake.protocolVersion = 1;
    fake.helloMetadata = false;
    const other = new FakeAdapter({ adapterId: "BBBB0002" });
    const { c, deps } = controller({ "/a": fake }, { openProxy: async (_path, native) => ({ stream: other.open(), hello: { ...native, serialNumber: fake.id } }) });
    try {
      await c.manager.rescan();
      await until(() => deps.log.mock.calls.some(([m]) => m === "/a: another adapter answered through the Cordial CLI"));
      expect(c.manager.connected.size).toBe(0);
    } finally {
      await c.stop();
    }
  });
});

describe("suspending an adapter opened through the Cordial CLI", () => {
  const settle = () => new Promise((r) => setTimeout(r, 3 * EXIT_MS));

  /** Makes the harness's next CLI hold its converted handshake until `release` is called. */
  function holdNext({ fake, helper }: Awaited<ReturnType<typeof start>>) {
    const impl = helper.openProxy.getMockImplementation()!;
    const held = { release: () => {} };
    helper.openProxy.mockImplementationOnce(async (path, native, signal) => {
      held.release = fake.hold();
      return impl(path, native, signal);
    });
    return held;
  }

  it("waits for a CLI still starting to exit", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = 1;
    let exited = false;
    const openProxy = vi.fn((_path: string, _native: AdapterHello, signal: AbortSignal) => new Promise<never>((_resolve, reject) => {
      signal.addEventListener("abort", () => setTimeout(() => {
        exited = true;
        reject(new Error("opening cancelled"));
      }, EXIT_MS));
    }));
    const { c, ports } = controller({ "/a": fake }, { openProxy });
    try {
      await c.manager.rescan();
      await until(() => openProxy.mock.calls.length === 1);
      await c.manager.suspend();
      expect(exited).toBe(true);
      expect(c.manager.refused.size).toBe(0);
      c.manager.burst();
      await c.manager.rescan();
      await settle();
      expect(ports.opened).toBe(1);
      expect(openProxy).toHaveBeenCalledOnce();
    } finally {
      await c.stop();
    }
  });

  it("waits for a CLI that became ready as suspending cancelled it to exit", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = 1;
    const helper = cli(fake, []);
    let ready = () => {};
    const openProxy = vi.fn((path: string, native: AdapterHello, signal: AbortSignal) =>
      new Promise<{ stream: ByteStream; hello: AdapterHello }>((resolve) => (ready = () => resolve(helper.openProxy(path, native, signal)))));
    const { c } = controller({ "/a": fake }, { openProxy });
    try {
      await c.manager.rescan();
      await until(() => openProxy.mock.calls.length === 1);
      ready();
      const suspending = c.manager.suspend();
      expect(helper.alive()).toBe(1);
      await suspending;
      expect(helper.alive()).toBe(0);
      expect(fake.hellos).toBe(0);
      expect(c.manager.connected.size).toBe(0);
    } finally {
      await c.stop();
    }
  });

  it("cancels the converted handshake and waits for the CLI to exit", async () => {
    const h = await start();
    const { fake, events, helper, c } = h;
    const held = holdNext(h);
    try {
      await c.manager.rescan();
      await until(() => helper.alive() === 1 && fake.hellos === 1);
      await c.manager.suspend();
      expect(helper.alive()).toBe(0);
      expect(events.at(-1)).toBe("cli exit");
      held.release();
      await settle();
      expect(c.manager.connected.size).toBe(0);
      expect(fake.received).toEqual([]);
    } finally {
      await c.stop();
    }
  });

  it("closes an established session, waits for the CLI to exit, and resumes with a fresh direct handshake", async () => {
    const { fake, events, helper, c } = await start();
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      await c.manager.suspend();
      expect(helper.alive()).toBe(0);
      expect(c.manager.connected.size).toBe(0);
      expect(await c.manager.connect(fake.id)).toBe(MIGRATION_TEXT.busy);
      await settle();
      expect(events).toEqual(["direct open", "direct close", "cli start", "cli exit"]);
      c.manager.resume();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      expect(events).toEqual(["direct open", "direct close", "cli start", "cli exit", "direct open", "direct close", "cli start"]);
    } finally {
      await c.stop();
    }
  });

  it("stops recovering after the CLI's exit and opens nothing until resumed", async () => {
    const { fake, events, helper, c } = await start();
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      helper.started[0]!.die();
      await until(() => c.state().adapters[0]?.connection === "reconnecting");
      await c.manager.suspend();
      expect(helper.alive()).toBe(0);
      await new Promise((r) => setTimeout(r, 600));
      expect(events).toEqual(["direct open", "direct close", "cli start", "cli exit"]);
      expect(c.state().adapters).toEqual([]);
      c.manager.resume();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      expect(events.slice(4)).toEqual(["direct open", "direct close", "cli start"]);
    } finally {
      await c.stop();
    }
  });

  it("ends a Connect through the CLI as busy once the CLI has exited", async () => {
    const h = await start();
    const { fake, helper, c } = h;
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      await c.manager.disconnect(fake.id);
      expect(helper.alive()).toBe(0);
      const held = holdNext(h);
      const connecting = c.manager.connect(fake.id);
      await until(() => helper.alive() === 1 && fake.hellos === 1);
      await c.manager.suspend();
      expect(helper.alive()).toBe(0);
      expect(await connecting).toBe(MIGRATION_TEXT.busy);
      held.release();
      expect(c.manager.connected.size).toBe(0);
      expect(c.manager.disconnected.get(fake.id)).toMatchObject({ connecting: false });
    } finally {
      await c.stop();
    }
  });
});
