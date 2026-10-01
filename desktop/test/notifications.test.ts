import { afterEach, expect, it, vi } from "vitest";
import { closeNotifications, showNotification } from "../src/main/notifications.ts";

const mock = vi.hoisted(() => ({
  references: [] as WeakRef<import("node:events").EventEmitter>[],
  closed: 0,
}));

vi.mock("electron", async () => {
  const { EventEmitter } = await import("node:events");
  return {
    Notification: class extends EventEmitter {
      static isSupported() { return true; }
      constructor() {
        super();
        mock.references.push(new WeakRef(this));
      }
      show() {}
      close() {
        mock.closed++;
        this.emit("close", { reason: "applicationHidden" });
      }
    },
  };
});

afterEach(() => {
  closeNotifications();
  mock.references = [];
  mock.closed = 0;
});

async function collect() {
  await new Promise((r) => setTimeout(r, 0));
  for (let i = 0; i < 5; i++) {
    global.gc!();
    await new Promise((r) => setTimeout(r, 10));
  }
}

it.skipIf(!global.gc)("keeps notification click routing alive through garbage collection", async () => {
  const clicked = vi.fn();
  showNotification({ title: "Battery" }, clicked);
  await collect();
  expect(mock.references[0]!.deref()).toBeDefined();
  mock.references[0]!.deref()!.emit("click");
  expect(clicked).toHaveBeenCalledOnce();
  await collect();
  expect(mock.references[0]!.deref()).toBeUndefined();
});

it.skipIf(!global.gc)("keeps timed-out notifications clickable in Action Center", async () => {
  const clicked = vi.fn();
  showNotification({ title: "Battery" }, clicked);
  mock.references[0]!.deref()!.emit("close", { reason: "timedOut" });
  await collect();
  expect(mock.references[0]!.deref()).toBeDefined();
  mock.references[0]!.deref()!.emit("click");
  expect(clicked).toHaveBeenCalledOnce();
});

it.skipIf(!global.gc).each(["close", "failed"])("releases notifications after %s", async (event) => {
  showNotification({ title: "Battery" });
  mock.references[0]!.deref()!.emit(event, {});
  await collect();
  expect(mock.references[0]!.deref()).toBeUndefined();
});

it("bounds notification ownership when final dismissals are not reported", () => {
  for (let i = 0; i < 65; i++) showNotification({ title: "Battery" });
  expect(mock.closed).toBe(1);
  closeNotifications();
  expect(mock.closed).toBe(65);
});
