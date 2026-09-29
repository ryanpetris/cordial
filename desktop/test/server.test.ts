import { createServer, request, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startBackend, type Backend } from "../src/server/backend.ts";
import type { AppState } from "../src/shared/state.ts";
import { until } from "./helpers.ts";

let backend: Backend;
let server: Server;
let port: number;

beforeAll(async () => {
  backend = await startBackend(2);
  server = createServer((req, res) => {
    if (!backend.handle(req, res)) res.writeHead(404).end();
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  port = (server.address() as AddressInfo).port;
});

afterAll(async () => {
  await backend.stop();
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
});

/** A request with full control of the headers; fetch won't set Host. */
function call(method: string, path: string, headers: Record<string, string> = {}, body?: string) {
  return new Promise<{ status: number; body: string }>((resolve, reject) => {
    const req = request({ host: "127.0.0.1", port, method, path, headers: { host: `localhost:${port}`, ...headers } }, (res) => {
      let text = "";
      res.on("data", (chunk) => (text += chunk));
      res.on("end", () => resolve({ status: res.statusCode!, body: text }));
    });
    req.on("error", reject);
    req.end(body);
  });
}

const act = (action: unknown) => call("POST", "/api/act", { "content-type": "application/json" }, JSON.stringify(action));
const state = async () => JSON.parse((await call("GET", "/api/state")).body) as AppState;

describe("development server", () => {
  it("serves the state and performs actions", async () => {
    expect((await state()).adapters.map((a) => a.id)).toEqual(["0000FAKE0001", "0000FAKE0002"]);
    expect(await act({ type: "adapters.refresh" })).toEqual({ status: 200, body: '{"ok":true}' });
    expect((await act({ type: "nope" })).status).toBe(400);
    expect((await call("POST", "/api/act", { "content-type": "application/json" }, "{")).status).toBe(400);
    expect((await call("GET", "/api/other")).status).toBe(404);
  });

  it("refuses requests another site could make", async () => {
    expect(await call("GET", "/api/state", { host: `attacker.example:${port}` })).toEqual({ status: 403, body: '{"error":"unexpected Host"}' });
    expect(await call("GET", "/api/state", { origin: "http://attacker.example" })).toEqual({ status: 403, body: '{"error":"unexpected Origin"}' });
    expect((await call("GET", "/api/state", { origin: `http://localhost:${port}` })).status).toBe(200);
    const plain = await call("POST", "/api/act", { "content-type": "text/plain" }, '{"type":"adapters.refresh"}');
    expect(plain).toEqual({ status: 403, body: '{"error":"expected JSON"}' });
  });

  it("streams the state and stops a search when the last page leaves", async () => {
    const received: string[] = [];
    const req = request({ host: "127.0.0.1", port, path: "/api/events", headers: { host: `localhost:${port}` } }, (res) =>
      res.on("data", (chunk) => received.push(String(chunk))),
    );
    req.end();
    await until(() => received.some((c) => c.startsWith("data: {")));
    expect(await act({ type: "scan.start", adapterId: "0000FAKE0001" })).toEqual({ status: 200, body: '{"ok":true}' });
    await until(() => received.some((c) => c.includes('"running":true')));
    req.destroy();
    // Well before the simulated scan ends by itself after a second.
    let stopped = false;
    for (let i = 0; i < 20 && !stopped; i++) {
      stopped = !(await state()).scan?.running;
      if (!stopped) await new Promise((r) => setTimeout(r, 20));
    }
    expect(stopped).toBe(true);
  });
});
