import { describe, expect, it } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { isAction } from "../src/core/actions.ts";
import { statusProblem } from "../src/protocol/status.ts";
import { controller, openSession, until } from "./helpers.ts";

const settled = (s: Awaited<ReturnType<typeof openSession>>["session"]) =>
  s.view.valid && s.view.infoNeeded().length === 0;

describe("adapter session and controller behavior", () => {
  it("retries a busy settings list and reports a persistent failure", async () => {
    const fake = new FakeAdapter();
    fake.failures["hidpp.setting.list"] = ["busy"];
    const { session } = await openSession({});
    await session.close();
    const { session: s } = await (async () => {
      const f = fake;
      const { AdapterSession } = await import("../src/core/session.ts");
      const hooks = { changed: () => {}, closed: () => {}, log: () => {} };
      const s = await AdapterSession.open(f.open(), hooks);
      s.run();
      return { session: s };
    })();
    await until(() => settled(s));
    s.watchSettings(["d_1"]);
    await until(() => !!s.view.settings("d_1")?.current);
    expect(s.view.settings("d_1")!.settings.length).toBe(6);
    fake.failures["hidpp.setting.list"] = ["settings_unavailable"];
    s.watchSettings(["d_2"]);
    await until(() => !!s.view.settings("d_2")?.loadError?.includes("hasn't read"));
    expect(s.view.settings("d_2")!.current).toBe(false);
    await s.close();
  });

  it("replaces information with complete snapshots, keeping names", async () => {
    const { fake, session } = await openSession();
    await until(() => settled(session));
    const d = fake.find("d_1");
    d.info = d.info.map((f) =>
      f.key === "serial" || f.key === "name" ? { ...f, value: null, available: false, fresh: false } : f,
    );
    await session.refreshInfo("d_1");
    const info = session.view.info("d_1")!;
    expect(info.find((f) => f.key === "serial")?.available).toBe(false);
    expect(session.view.reportedName("d_1")).toBe("Example Keys Wireless");
    await session.close();
  });

  it("ignores input left from an earlier session", async () => {
    const fake = new FakeAdapter();
    fake.staleInput = '{"v":1,"type":"response","id":7,"ok":true,"done":true,"result":{}}\npartial{';
    const { AdapterSession } = await import("../src/core/session.ts");
    const s = await AdapterSession.open(fake.open(), { changed: () => {}, closed: () => {}, log: () => {} });
    expect(s.adapterId).toBe(fake.id);
    await s.close();
  });

  it("forgets a dismissed pairing and scans tied to a closed session", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", pairingMethod: "enter_passkey" });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => state()?.devices.length === 4);
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: "c_1" });
    await until(() => !!state()!.pairing?.prompt);
    await Promise.all([c.act({ type: "pair.cancel" }), c.act({ type: "pair.dismiss" }), c.act({ type: "scan.stop" })]);
    await new Promise((r) => setTimeout(r, 100));
    expect(c.state().pairing).toBeNull();
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => c.state().scan?.running === true);
    await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" });
    expect(c.state().scan).toBeNull();
    await c.stop();
  });

  it("hides an adapter whose USB serial isn't its ID", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, ports } = controller({ "/a": a });
    ports.list = async () => [{ path: "/a", serial: "" }];
    await c.manager.rescan();
    expect(c.state().adapters).toEqual([]);
    expect(deps.log).toHaveBeenCalledWith(expect.stringContaining("ignoring it"));
    await c.stop();
  });

  it("keeps a disconnected adapter's port closed across rescans", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, ports, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => state()?.devices.length === 4);
    await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" });
    const opened = ports.opened;
    await c.manager.rescan();
    c.manager.burst();
    await new Promise((r) => setTimeout(r, 50));
    expect(ports.opened).toBe(opened);
    expect(c.state().adapters[0]?.connection).toBe("disconnected");
    await c.stop();
  });

  it("shows why information and settings couldn't be read", async () => {
    const fake = new FakeAdapter();
    fake.failures["device.info"] = ["internal_error"];
    fake.failures["hidpp.setting.list"] = ["busy", "busy", "busy", "busy"];
    const { AdapterSession } = await import("../src/core/session.ts");
    const s = await AdapterSession.open(fake.open(), { changed: () => {}, closed: () => {}, log: () => {} });
    s.run();
    await until(() => !!s.view.infoError("d_1"));
    s.watchSettings(["d_1"]);
    await until(() => !!s.view.settings("d_1")?.loadError, 6000);
    expect(s.view.settings("d_1")!.current).toBe(false);
    await s.close();
  }, 10000);

  it("doesn't repeat a low-battery alert after the adapter reconnects", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => deps.lowBattery.mock.calls.length === 1);
    await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" });
    await c.act({ type: "adapter.connect", adapterId: "AAAA0001" });
    await until(() => !!state()?.devices.find((d) => d.key === "AAAA0001/d_2")?.battery);
    await new Promise((r) => setTimeout(r, 100));
    expect(deps.lowBattery).toHaveBeenCalledTimes(1);
    await c.stop();
  });

  it("validates IPC actions and adapter status", () => {
    expect(isAction({ type: "device.connect", key: "A/d_1" })).toBe(true);
    expect(isAction({ type: "device.connect" })).toBe(false);
    expect(isAction({ type: "device.connect", key: "x", extra: 1 })).toBe(false);
    expect(isAction({ type: "preferences", preferences: { lowBatteryPercent: 30 } })).toBe(true);
    expect(isAction({ type: "preferences", preferences: { unknown: true } })).toBe(false);
    expect(isAction({ type: "setting.set", key: "k", setting: "wheel.mode", value: 1.5 })).toBe(false);
    const status = new FakeAdapter().status();
    expect(statusProblem(status, ["classic", "ble"])).toBeNull();
    expect(statusProblem(status, ["ble"])).toMatch(/capacity/);
    expect(statusProblem({ ...status, session_id: "" }, ["classic", "ble"])).toMatch(/identity/);
  });
});
