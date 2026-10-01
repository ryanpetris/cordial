import { afterEach, expect, it, vi } from "vitest";
import { watchHotplug } from "../src/node/host.ts";

interface Watcher {
  attach?: () => void;
  detach?: () => void;
}

const mock = vi.hoisted(() => ({ references: [] as WeakRef<Watcher>[], failDetach: false }));
vi.mock("usb/index.js", () => ({
  Emitter: class implements Watcher {
    attach?: () => void;
    detach?: () => void;
    constructor() { mock.references.push(new WeakRef(this)); }
    async addAttach(callback: () => void) { this.attach = callback; }
    async addDetach(callback: () => void) {
      if (mock.failDetach) throw new Error("watch failed");
      this.detach = callback;
    }
    async removeAttach() { this.attach = undefined; }
    async removeDetach() { this.detach = undefined; }
  },
}));

afterEach(() => { mock.references = []; mock.failDetach = false; });

function emitHotplug() {
  mock.references[0]!.deref()!.attach!();
  mock.references[0]!.deref()!.detach!();
}

it.skipIf(!global.gc)("owns the USB watcher through garbage collection until stopped", async () => {
  const changed = vi.fn();
  const stop = await watchHotplug(changed, vi.fn());
  expect(stop).not.toBeNull();
  await new Promise((r) => setTimeout(r, 0));
  global.gc!();
  emitHotplug();
  expect(changed).toHaveBeenCalledTimes(2);
  changed.mockClear();
  await stop!();
  await stop!();
  for (let i = 0; i < 5; i++) {
    global.gc!();
    await new Promise((r) => setTimeout(r, 10));
  }
  expect(mock.references[0]!.deref()).toBeUndefined();
});

it("cleans up a partially installed watcher", async () => {
  mock.failDetach = true;
  const log = vi.fn();
  expect(await watchHotplug(vi.fn(), log)).toBeNull();
  expect(mock.references[0]!.deref()!.attach).toBeUndefined();
  expect(log).toHaveBeenCalledOnce();
});
