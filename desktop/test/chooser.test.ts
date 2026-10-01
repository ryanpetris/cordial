import { afterEach, expect, it, vi } from "vitest";

afterEach(() => { vi.unstubAllGlobals(); vi.resetModules(); });

it("keeps chooser dismissal quiet and returns genuine chooser failures", async () => {
  const choosePort = vi.fn();
  vi.stubGlobal("window", { cordial: { host: { choosePort } } });
  const { chooseAdapter } = await import("../src/renderer/api.ts");
  choosePort.mockRejectedValueOnce(new DOMException("No port selected", "NotFoundError"));
  expect(await chooseAdapter()).toEqual({ ok: true });
  choosePort.mockRejectedValueOnce(new DOMException("Port selection is unavailable", "SecurityError"));
  expect(await chooseAdapter()).toEqual({ ok: false, message: "Port selection is unavailable" });
  choosePort.mockResolvedValueOnce(undefined);
  expect(await chooseAdapter()).toEqual({ ok: true });
});

it("propagates serial chooser failures through the web backend", async () => {
  const requestPort = vi.fn().mockRejectedValue(new DOMException("Port selection is unavailable", "SecurityError"));
  vi.stubGlobal("navigator", { platform: "Linux", serial: { requestPort, getPorts: async () => [], addEventListener: vi.fn() } });
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: vi.fn() });
  const listeners = new Map<string, (event: { persisted: boolean }) => void>();
  vi.stubGlobal("window", { addEventListener: (event: string, listener: (event: { persisted: boolean }) => void) => listeners.set(event, listener) });
  const { startWebBackend } = await import("../src/web/backend.ts");
  const api = await startWebBackend(0);
  await expect(api.host.choosePort!()).rejects.toMatchObject({ name: "SecurityError" });
  listeners.get("pagehide")!({ persisted: false });
});

it("keeps transport failures in the action's feedback surface", async () => {
  vi.stubGlobal("window", { cordial: { act: vi.fn().mockRejectedValue(new Error("IPC unavailable")) } });
  const { act, setReporter } = await import("../src/renderer/api.ts");
  const report = vi.fn();
  setReporter(report);
  expect(await act({ type: "device.refresh", key: "A/d_1" }, true)).toMatchObject({ ok: false });
  expect(report).not.toHaveBeenCalled();
  expect(await act({ type: "adapters.refresh" })).toMatchObject({ ok: false });
  expect(report).toHaveBeenCalledExactlyOnceWith("Couldn't confirm that action.");
});
