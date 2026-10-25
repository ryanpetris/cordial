import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { ErrorCode, ErrorOutcome, ErrorReason, MigrationEventSchema, StatusSchema, type ErrorSchema } from "@cordial/protocol";
import type { ByteStream } from "@cordial/client";
import { describe, expect, it, vi } from "vitest";
import type { Controller } from "../src/core/controller.ts";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { isAction } from "../src/core/actions.ts";
import { storageGate, type MigrationHandlers, type MigrationHost, type MigrationProcess } from "../src/core/migration.ts";
import { MIGRATION_TEXT, adapterStatus, canRestart, errorText } from "../src/shared/text.ts";
import { controller, until } from "./helpers.ts";

type ErrorInit = MessageInitShape<typeof ErrorSchema>;

const MARKER: ErrorInit = { code: ErrorCode.STORAGE_MIGRATION_REQUIRED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.NO_EFFECT };
const RADIO: ErrorInit = { code: ErrorCode.NOT_READY, reason: ErrorReason.RADIO };
const STORAGE: ErrorInit = { code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE };

const status = (target: number | null | undefined, errors: ErrorInit[] = [], storageFormat?: number, ready = false) =>
  create(StatusSchema, { id: "000000000000A001", ready, errors, information: target === null ? undefined : { targetStorageFormat: target, storageFormat } });

describe("storage gate", () => {
  it.each([
    ["no marker at the target", status(2, [], 2, true), { kind: "current" }],
    ["no marker with an older reported format", status(2, [], 1), { kind: "current" }],
    ["no marker with an unknown format, even when ready", status(2, [], undefined, true), { kind: "current" }],
    ["no marker with blocked storage", status(2, [STORAGE], 1), { kind: "current" }],
    ["the marker alone", status(2, [MARKER], 1), { kind: "assisted" }],
    ["the marker after the root reached the target", status(2, [MARKER], 2), { kind: "assisted" }],
    ["the marker with an unknown format", status(2, [MARKER]), { kind: "assisted" }],
    ["the marker and a radio error", status(2, [RADIO, MARKER], 1), { kind: "blocked", error: { code: "not_ready", reason: "radio", outcome: "unspecified", related: [] } }],
    ["the marker and a storage failure", status(2, [STORAGE, MARKER]), { kind: "blocked", error: { code: "storage_failed", reason: "storage", outcome: "unspecified", related: [] } }],
    ["a marker with an unknown outcome", status(2, [{ ...MARKER, outcome: ErrorOutcome.UNKNOWN }], 1), { kind: "blocked", error: { code: "storage_migration_required", reason: "storage", outcome: "unknown", related: [] } }],
    ["a marker with related failures", status(2, [{ ...MARKER, related: [STORAGE] }], 1), { kind: "blocked", error: expect.objectContaining({ code: "storage_migration_required", related: [expect.objectContaining({ code: "storage_failed" })] }) }],
    ["a marker for another reason", status(2, [{ ...MARKER, reason: ErrorReason.RADIO }], 1), { kind: "blocked", error: expect.objectContaining({ code: "storage_migration_required", reason: "radio" }) }],
    ["a newer target with the marker", status(3, [MARKER], 1), { kind: "unsupported" }],
    ["an older target", status(1, [], 1, true), { kind: "unsupported" }],
    ["a zero target", status(0, [], 2, true), { kind: "unknown" }],
    ["an absent target in present information while ready", status(undefined, [], 2, true), { kind: "unknown" }],
    ["an absent target in present information with the marker", status(undefined, [MARKER], 2, true), { kind: "unknown" }],
    ["no adapter information", status(null, [MARKER]), { kind: "unknown" }],
  ])("%s", (_, s, gate) => {
    expect(storageGate(s)).toEqual(gate);
  });

  it("words the marker as an ordinary error", () => {
    expect(errorText({ code: "storage_migration_required", reason: "storage", outcome: "no_effect", related: [] })).toBe("The adapter's saved data must be migrated before use.");
  });
});

it("validates migration and restart actions", () => {
  expect(isAction({ type: "adapter.restart", adapterId: "000000000000A001" })).toBe(true);
  expect(isAction({ type: "adapter.restart" })).toBe(false);
  expect(isAction({ type: "migration.start", adapterId: "000000000000A001" })).toBe(true);
  expect(isAction({ type: "migration.start" })).toBe(false);
  expect(isAction({ type: "migration.start", adapterId: 1 })).toBe(false);
  expect(isAction({ type: "migration.cancel" })).toBe(true);
  expect(isAction({ type: "migration.cancel", adapterId: "000000000000A001" })).toBe(false);
});

/** A migration host whose runs the test drives. */
function fakeHost(cli: string | null = "/usr/bin/cordial") {
  const runs: { handlers: MigrationHandlers; sent: (string | undefined)[]; openAtStart: number }[] = [];
  let open = () => 0;
  const host: MigrationHost & { cli: string | null } = {
    cli,
    find: vi.fn(() => host.cli),
    start: vi.fn((_path: string, handlers: MigrationHandlers): MigrationProcess => {
      const run = { handlers, sent: [] as (string | undefined)[], openAtStart: open() };
      runs.push(run);
      return { send: (c) => void run.sent.push(c.action?.case) };
    }),
  };
  return { host, runs, countOpen: (count: () => number) => (open = count) };
}

const send = (run: { handlers: MigrationHandlers }, event: MessageInitShape<typeof MigrationEventSchema>["event"]) =>
  run.handlers.event(create(MigrationEventSchema, { event }));

/** An adapter that started in assisted mode with a source format of 1. */
const needsMigration = (adapterId = "000000000000A001") => {
  const fake = new FakeAdapter({ adapterId });
  fake.storageFormat = 1;
  fake.migrationRequired = true;
  return fake;
};

/** The CLI's writes and restart: the root reaches the target and startup leaves assisted mode. */
const migrated = async (fake: FakeAdapter) => {
  fake.storageFormat = 2;
  fake.reboot();
  await until(() => fake.present);
};

/** One adapter needing migration at /a and a working one at /b, with every open stream counted. */
async function harness(cli: string | null = "/usr/bin/cordial", withHost = true) {
  const a = needsMigration();
  const b = new FakeAdapter({ adapterId: "000000000000B001" });
  const fake = fakeHost(cli);
  const h = controller({ "/a": a, "/b": b }, withHost ? { migration: fake.host } : {});
  let open = 0;
  h.ports.open = async (path) => {
    const stream = h.deps.ports[path]!.open();
    open++;
    let closed = false;
    const done = () => {
      if (!closed) open--;
      closed = true;
    };
    stream.onClose(done);
    return { ...bind(stream), close: async () => { await stream.close(); done(); } };
  };
  fake.countOpen(() => open);
  await h.c.manager.rescan();
  return { a, b, ...fake, ...h, open: () => open };
}

const bind = (s: FakeAdapter): ByteStream => ({
  onData: (l) => s.onData(l),
  onClose: (l) => s.onClose(l),
  write: (bytes) => s.write(bytes),
  close: () => s.close(),
});

const entry = (h: { c: Controller }, id = "000000000000A001") => h.c.state().adapters.find((x) => x.id === id);

describe("adapters that can't use their saved data yet", () => {
  it.each([
    ["an unsupported target", (f: FakeAdapter) => (f.targetStorageFormat = 3), MIGRATION_TEXT.unsupported],
    ["an unsupported target in assisted mode", (f: FakeAdapter) => ((f.targetStorageFormat = 3), (f.migrationRequired = true)), MIGRATION_TEXT.unsupported],
    ["firmware without a target", (f: FakeAdapter) => (f.targetStorageFormat = null), MIGRATION_TEXT.firmware],
    ["firmware without a target with the marker", (f: FakeAdapter) => ((f.targetStorageFormat = null), (f.migrationRequired = true)), MIGRATION_TEXT.firmware],
  ])("get nothing but the opening status with %s", async (_, setup, message) => {
    const fake = new FakeAdapter({ ready: true });
    setup(fake);
    if (fake.targetStorageFormat === null) {
      expect(fake.status().information).toEqual(expect.objectContaining({ storageFormat: 2 }));
      expect(fake.status().information?.targetStorageFormat).toBeUndefined();
      expect(fake.status().ready).toBe(!fake.migrationRequired);
    }
    const host = fakeHost();
    const { c } = controller({ "/a": fake }, { migration: host.host });
    try {
      await c.manager.rescan();
      await until(() => c.state().adapters.length === 1);
      await new Promise((r) => setTimeout(r, 50));
      expect(c.state().adapters[0]!.migration).toEqual({ phase: "unsupported", message, progress: null, canStart: false, canCancel: false });
      expect(adapterStatus(c.state().adapters[0]!)).toEqual({ text: "Unsupported", warn: true });
      for (const action of [
        { type: "adapter.name", adapterId: fake.id, name: "x" },
        { type: "devices.reload", adapterId: fake.id },
        { type: "scan.start", adapterId: fake.id },
        { type: "profiles.page", adapterId: fake.id, page: "first" },
        { type: "adapter.restart", adapterId: fake.id },
        { type: "migration.start", adapterId: fake.id },
      ] as const)
        expect(await c.act(action)).toEqual({ ok: false, message });
      expect(canRestart(c.state().adapters[0]!)).toBe(false);
      expect(fake.received.map((r) => r.command.case)).toEqual(["getStatus"]);
      expect(host.host.find).not.toHaveBeenCalled();
      expect(host.host.start).not.toHaveBeenCalled();
      expect(fake.restarts).toBe(0);
    } finally {
      await c.stop();
    }
  });

  it.each([
    ["while it starts", [], 2],
    ["while its storage is blocked", [STORAGE], 2],
    ["while its storage is blocked at an older format", [STORAGE], 1],
    ["while its format is unknown", [STORAGE], null],
  ])("lists nothing, migrates nothing and restarts nothing until an adapter without the marker is ready %s", async (_, errors, format) => {
    const fake = new FakeAdapter({ ready: false });
    fake.faults = errors;
    fake.storageFormat = format;
    const host = fakeHost();
    const { c } = controller({ "/a": fake }, { migration: host.host });
    try {
      await c.manager.rescan();
      await until(() => c.state().adapters.length === 1);
      await new Promise((r) => setTimeout(r, 50));
      expect(c.state().adapters[0]!.migration).toBeNull();
      expect(adapterStatus(c.state().adapters[0]!)).toEqual(errors.length ? { text: "Needs Attention", warn: true } : { text: "Starting…", warn: false });
      expect(await c.act({ type: "devices.reload", adapterId: fake.id })).toEqual({ ok: true });
      expect(fake.received.map((r) => r.command.case)).toEqual(["getStatus"]);
      expect(host.host.find).not.toHaveBeenCalled();
      expect(fake.restarts).toBe(0);
      expect(canRestart(c.state().adapters[0]!)).toBe(true);
      fake.changeAdapter({ ready: true });
      await until(() => c.state().adapters[0]!.readiness === "ready" && c.state().devices.length > 0);
    } finally {
      await c.stop();
    }
  });

  it("stays blocked without a host that can start the CLI", async () => {
    const h = await harness(null, false);
    try {
      await until(() => !!entry(h)?.migration && h.c.state().devices.length > 0);
      expect(entry(h)!.migration).toEqual({ phase: "required", message: MIGRATION_TEXT.cli, progress: null, canStart: false, canCancel: false });
      expect(h.a.received.map((r) => r.command.case)).toEqual(["getStatus"]);
      expect(await h.c.act({ type: "migration.start", adapterId: h.a.id })).toEqual({ ok: false, message: MIGRATION_TEXT.cli });
      expect(h.open()).toBe(2);
    } finally {
      await h.c.stop();
    }
  });

  it("stays blocked while the CLI isn't installed, and checks again on refresh", async () => {
    const h = await harness(null);
    try {
      await until(() => entry(h)?.migration?.message === MIGRATION_TEXT.cli);
      expect(h.host.find).toHaveBeenCalledTimes(1);
      expect(h.host.start).not.toHaveBeenCalled();
      expect(h.open()).toBe(2);
      expect(entry(h)!.migration!.canStart).toBe(true);
      h.host.cli = "/usr/bin/cordial";
      await h.c.act({ type: "adapters.refresh" });
      await until(() => h.runs.length === 1);
      expect(h.runs[0]!.openAtStart).toBe(0);
    } finally {
      void h.c.cancelMigration();
      h.runs[0]?.handlers.exit(0, null);
      await h.c.stop();
    }
  });
});

describe("assisted migration", () => {
  it("starts the CLI for the marker even when the root already declares the target", async () => {
    const h = await harness();
    h.a.storageFormat = 2;
    try {
      await until(() => h.runs.length === 1);
      expect(h.runs[0]!.openAtStart).toBe(0);
    } finally {
      void h.c.cancelMigration();
      h.runs[0]?.handlers.exit(1, null);
      await h.c.stop();
    }
  });

  it("starts nothing when another error comes with the marker, and reports that error", async () => {
    const a = needsMigration();
    a.faults = [RADIO];
    const host = fakeHost();
    const { c } = controller({ "/a": a }, { migration: host.host });
    try {
      await c.manager.rescan();
      await until(() => !!entry({ c })?.migration);
      await new Promise((r) => setTimeout(r, 50));
      const message = "The adapter's Bluetooth radio isn't ready. Try again in a moment.";
      expect(entry({ c })!.migration).toEqual({ phase: "required", message, progress: null, canStart: false, canCancel: false });
      expect(await c.act({ type: "migration.start", adapterId: a.id })).toEqual({ ok: false, message });
      expect(await c.act({ type: "devices.reload", adapterId: a.id })).toEqual({ ok: false, message });
      expect(host.host.find).not.toHaveBeenCalled();
      expect(a.received.map((r) => r.command.case)).toEqual(["getStatus"]);
    } finally {
      await c.stop();
    }
  });

  it("closes every adapter, then starts the CLI automatically with the adapter's ID", async () => {
    const h = await harness();
    try {
      await until(() => h.runs.length === 1);
      expect(h.runs[0]!.openAtStart).toBe(0);
      expect(h.runs[0]!.sent).toEqual(["start"]);
      expect(h.a.received.map((r) => r.command.case)).toEqual(["getStatus"]);
      expect(h.c.manager.connected.size).toBe(0);
      expect(h.c.state().adapters.map((x) => [x.id, x.migration?.phase])).toEqual([[h.a.id, "starting"]]);
    } finally {
      void h.c.cancelMigration();
      h.runs[0]?.handlers.exit(1, null);
      await h.c.stop();
    }
  });

  it("opens nothing while the CLI waits for the adapter, and resumes after verified success and exit", async () => {
    const h = await harness();
    const run = await until(() => h.runs.length === 1).then(() => h.runs[0]!);
    const opened = h.ports.opened;
    try {
      send(run, { case: "progress", value: { completedBytes: 10n, totalBytes: 40n } });
      expect(entry(h)!.migration).toMatchObject({ phase: "migrating", progress: { completed: 10, total: 40 }, canCancel: true });
      send(run, { case: "waiting", value: { adapterId: h.a.id } });
      expect(adapterStatus(entry(h)!)).toEqual({ text: "Waiting for Adapter", warn: false });
      h.c.manager.burst();
      await h.c.manager.rescan();
      expect(await h.c.act({ type: "adapter.connect", adapterId: h.b.id })).toEqual({ ok: false, message: MIGRATION_TEXT.busy });
      expect(await h.c.act({ type: "adapters.refresh" })).toEqual({ ok: false, message: MIGRATION_TEXT.busy });
      await new Promise((r) => setTimeout(r, 50));
      expect(h.ports.opened).toBe(opened);
      await migrated(h.a);
      send(run, { case: "success", value: { adapterId: h.a.id, targetFormat: 2 } });
      await new Promise((r) => setTimeout(r, 50));
      expect(h.ports.opened).toBe(opened);
      run.handlers.exit(0, null);
      await until(() => h.c.state().adapters.length === 2 && h.c.state().adapters.every((x) => x.readiness === "ready"));
      expect(h.c.state().adapters.every((x) => x.migration === null)).toBe(true);
      expect(h.runs).toHaveLength(1);
    } finally {
      await h.c.stop();
    }
  });

  it.each([
    ["a failure", (run: Run) => { send(run, { case: "failure", value: { reason: "x" } }); run.handlers.exit(0, null); }, MIGRATION_TEXT.failed],
    ["an unconfirmed write", (run: Run) => { send(run, { case: "failure", value: { reason: "x", outcomeUnknown: true } }); run.handlers.exit(1, null); }, MIGRATION_TEXT.unknown],
    ["success for another adapter", (run: Run) => { send(run, { case: "success", value: { adapterId: "000000000000B001", targetFormat: 2 } }); run.handlers.exit(0, null); }, MIGRATION_TEXT.failed],
    ["success at another format", (run: Run) => { send(run, { case: "success", value: { adapterId: "000000000000A001", targetFormat: 3 } }); run.handlers.exit(0, null); }, MIGRATION_TEXT.failed],
    ["success with a failing exit", (run: Run) => { send(run, { case: "success", value: { adapterId: "000000000000A001", targetFormat: 2 } }); run.handlers.exit(1, null); }, MIGRATION_TEXT.failed],
    ["success and a signal", (run: Run) => { send(run, { case: "success", value: { adapterId: "000000000000A001", targetFormat: 2 } }); run.handlers.exit(null, "SIGKILL"); }, MIGRATION_TEXT.failed],
    ["an exit without a result", (run: Run) => run.handlers.exit(0, null), MIGRATION_TEXT.failed],
    ["unusable output", (run: Run) => { run.handlers.malformed("bad"); send(run, { case: "success", value: { adapterId: "000000000000A001", targetFormat: 2 } }); run.handlers.exit(0, null); }, MIGRATION_TEXT.failed],
    ["an unknown event", (run: Run) => { run.handlers.event(create(MigrationEventSchema, {})); run.handlers.exit(0, null); }, MIGRATION_TEXT.failed],
    ["a CLI that couldn't start", (run: Run) => run.handlers.exit(null, null), MIGRATION_TEXT.failed],
  ])("stays suspended after %s until the user cancels", async (_, end, message) => {
    const h = await harness();
    const run = await until(() => h.runs.length === 1).then(() => h.runs[0]!);
    const opened = h.ports.opened;
    try {
      end(run);
      expect(entry(h)!.migration).toEqual({ phase: "failed", message, progress: null, canStart: false, canCancel: true });
      expect(adapterStatus(entry(h)!)).toEqual({ text: "Needs Attention", warn: true });
      h.c.manager.burst();
      await new Promise((r) => setTimeout(r, 50));
      expect(h.ports.opened).toBe(opened);
      expect(h.c.state().adapters).toHaveLength(1);
      await h.c.act({ type: "migration.cancel" });
      await until(() => h.c.state().adapters.length === 2);
      // A cancelled adapter waits for Migrate instead of starting again by itself.
      await until(() => entry(h)?.migration?.phase === "required");
      expect(entry(h)!.migration!.canStart).toBe(true);
      await new Promise((r) => setTimeout(r, 50));
      expect(h.runs).toHaveLength(1);
      expect(run.sent).toEqual(["start"]);
    } finally {
      await h.c.stop();
    }
  });

  it("cancels by asking the CLI and waits for it to exit", async () => {
    const h = await harness();
    const run = await until(() => h.runs.length === 1).then(() => h.runs[0]!);
    const opened = h.ports.opened;
    try {
      let done = false;
      void h.c.cancelMigration().then(() => (done = true));
      expect(await h.c.act({ type: "migration.cancel" })).toEqual({ ok: true });
      expect(run.sent).toEqual(["start", "cancel"]);
      expect(entry(h)!.migration!.phase).toBe("cancelling");
      send(run, { case: "cancelled", value: {} });
      await new Promise((r) => setTimeout(r, 50));
      expect(done).toBe(false);
      expect(h.ports.opened).toBe(opened);
      run.handlers.exit(1, null);
      await until(() => done && h.c.state().adapters.length === 2);
      // Migrate starts it again.
      await until(() => entry(h)?.migration?.canStart === true);
      expect(await h.c.act({ type: "migration.start", adapterId: h.a.id })).toEqual({ ok: true });
      expect(h.runs).toHaveLength(2);
      expect(h.runs[1]!.openAtStart).toBe(0);
    } finally {
      void h.c.cancelMigration();
      h.runs[1]?.handlers.exit(1, null);
      await h.c.stop();
    }
  });

  it("stops only after a cancelled CLI exits, and opens nothing again", async () => {
    const h = await harness();
    const run = await until(() => h.runs.length === 1).then(() => h.runs[0]!);
    const opened = h.ports.opened;
    let stopped = false;
    void h.c.stop().then(() => (stopped = true));
    expect(run.sent).toEqual(["start", "cancel"]);
    await new Promise((r) => setTimeout(r, 50));
    expect(stopped).toBe(false);
    run.handlers.exit(0, null);
    await until(() => stopped);
    await new Promise((r) => setTimeout(r, 50));
    expect(h.ports.opened).toBe(opened);
  });

  it("lets a change in flight finish before closing its adapter", async () => {
    const h = await harness(null);
    try {
      await until(() => h.c.state().adapters.some((x) => x.id === h.b.id && x.readiness === "ready"));
      h.host.cli = "/usr/bin/cordial";
      const release = h.b.hold();
      const renaming = h.c.act({ type: "adapter.name", adapterId: h.b.id, name: "Kept" });
      await until(() => h.b.received.some((r) => r.command.case === "setAdapter"));
      const starting = h.c.act({ type: "migration.start", adapterId: h.a.id });
      expect(await h.c.act({ type: "adapter.name", adapterId: h.b.id, name: "Late" })).toEqual({ ok: false, message: MIGRATION_TEXT.busy });
      await new Promise((r) => setTimeout(r, 50));
      expect(h.runs).toHaveLength(0);
      release();
      expect(await renaming).toEqual({ ok: true });
      expect(await starting).toEqual({ ok: true });
      expect(h.b.name).toBe("Kept");
      expect(h.runs[0]!.openAtStart).toBe(0);
    } finally {
      void h.c.cancelMigration();
      h.runs[0]?.handlers.exit(1, null);
      await h.c.stop();
    }
  });

  it("cancels before the CLI starts without starting it", async () => {
    const h = await harness(null);
    try {
      await until(() => h.c.state().adapters.some((x) => x.id === h.b.id && x.readiness === "ready"));
      h.host.cli = "/usr/bin/cordial";
      const release = h.b.hold();
      void h.c.act({ type: "adapter.name", adapterId: h.b.id, name: "Kept" });
      await until(() => h.b.received.some((r) => r.command.case === "setAdapter"));
      const starting = h.c.act({ type: "migration.start", adapterId: h.a.id });
      await h.c.act({ type: "migration.cancel" });
      release();
      expect(await starting).toEqual({ ok: true });
      expect(h.runs).toHaveLength(0);
      await until(() => h.c.state().adapters.length === 2);
    } finally {
      await h.c.stop();
    }
  });
});

type Run = ReturnType<typeof fakeHost>["runs"][number];

describe("restart", () => {
  /** Restarts the adapter at /a and waits for it to open again. */
  async function restart(c: Controller, fake: FakeAdapter) {
    const opened = fake.restarts;
    expect(await c.act({ type: "adapter.restart", adapterId: fake.id })).toEqual({ ok: true });
    await until(() => fake.restarts === opened + 1 && fake.present);
    await c.manager.rescan();
    await until(() => entry({ c }, fake.id)?.connection === "connected" && fake.received.length > 0);
  }

  it("works on an adapter in assisted mode that something else blocks, which stays in assisted mode", async () => {
    const a = needsMigration();
    a.faults = [RADIO];
    const host = fakeHost();
    const { c } = controller({ "/a": a }, { migration: host.host });
    try {
      await c.manager.rescan();
      await until(() => !!entry({ c })?.migration);
      expect(canRestart(entry({ c })!)).toBe(true);
      await restart(c, a);
      await until(() => !!entry({ c })?.migration);
      expect(entry({ c })!.migration!.phase).toBe("required");
      expect(a.migrationRequired).toBe(true);
      expect(host.host.start).not.toHaveBeenCalled();
    } finally {
      await c.stop();
    }
  });

  it("keeps the marker after the root reaches the target until the adapter restarts", async () => {
    const a = needsMigration();
    const { c } = controller({ "/a": a });
    try {
      await c.manager.rescan();
      await until(() => entry({ c })?.migration?.phase === "required");
      a.storageFormat = 2;
      a.changeAdapter({});
      await new Promise((r) => setTimeout(r, 50));
      expect(entry({ c })!.migration!.phase).toBe("required");
      await restart(c, a);
      await until(() => entry({ c })?.readiness === "ready" && c.state().devices.length > 0);
      expect(entry({ c })!.migration).toBeNull();
    } finally {
      await c.stop();
    }
  });

  it("works without readiness and opens the adapter again", async () => {
    const fake = new FakeAdapter({ ready: false });
    const host = fakeHost();
    const { c } = controller({ "/a": fake }, { migration: host.host });
    try {
      await c.manager.rescan();
      await until(() => entry({ c }, fake.id)?.connection === "connected");
      await restart(c, fake);
      expect(fake.received.map((r) => r.command.case)).toEqual(["getStatus"]);
      expect(entry({ c }, fake.id)!.migration).toBeNull();
      expect(host.host.find).not.toHaveBeenCalled();
    } finally {
      await c.stop();
    }
  });
});
