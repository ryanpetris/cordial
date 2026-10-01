// Development server backend: runs the controller in Node, next to the
// adapters, and serves the window API over HTTP to a browser. For testing
// only; nothing here is built into the app or packaged. Listen on localhost
// and reach it through an SSH tunnel.
import type { IncomingMessage, ServerResponse } from "node:http";
import { isAction } from "../core/actions.ts";
import { Controller } from "../core/controller.ts";
import { listPorts, openSerial } from "@cordial/client/node";
import { hostPlatform, watchHotplug } from "../node/host.ts";
import { preferencesFrom, type AppState } from "../shared/state.ts";

const log = (message: string) => console.log(`[cordial] ${new Date().toISOString()} ${message}`);

/** Host names a local or tunnelled browser uses; anything else may be DNS rebinding. */
const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);
const MAX_BODY = 64 * 1024;

/** Why a request must be refused, or null. */
export function refusal(req: IncomingMessage): string | null {
  const host = req.headers.host ?? "";
  if (!LOCAL_HOSTS.has(host.replace(/:\d+$/, ""))) return "unexpected Host";
  // Browsers send Origin with cross-origin requests; only this page may call.
  const origin = req.headers.origin;
  if (origin !== undefined && origin !== `http://${host}`) return "unexpected Origin";
  // Another site can't send a JSON body without a CORS preflight, which is never approved.
  if (req.method === "POST" && req.headers["content-type"]?.split(";")[0]?.trim() !== "application/json") return "expected JSON";
  return null;
}

function reply(res: ServerResponse, status: number, body: unknown) {
  res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
  res.end(JSON.stringify(body));
}

async function body(req: IncomingMessage): Promise<unknown> {
  let text = "";
  for await (const chunk of req) {
    text += chunk;
    if (text.length > MAX_BODY) throw new Error("request too large");
  }
  return JSON.parse(text);
}

export interface Backend {
  /** Handles an /api/ request; false for any other path. */
  handle(req: IncomingMessage, res: ServerResponse): boolean;
  stop(): Promise<void>;
}

/** Starts the controller; `simulate` adapters replace the serial ports when above zero. */
export async function startBackend(simulate = 0): Promise<Backend> {
  const clients = new Set<ServerResponse>();
  const send = (res: ServerResponse, state: AppState) => res.write(`data: ${JSON.stringify(state)}\n\n`);
  const ports = simulate > 0 ? (await import("../fake/ports.ts")).simulatedPorts(simulate) : { listPorts, openTransport: openSerial };
  const controller = new Controller({
    ...ports,
    log,
    // Kept in memory: a test run leaves no settings behind.
    preferences: preferencesFrom(null),
    savePreferences: () => {},
    hostPlatform: hostPlatform(),
    published: (state) => {
      for (const res of clients) send(res, state);
    },
    lowBattery: (alert) => log(`notification: ${alert.name} battery ${alert.level}, ${alert.percent}%`),
    connection: (name, connected) => log(`notification: ${name} ${connected ? "connected" : "disconnected"}`),
  });
  const stopHotplug = !simulate ? await watchHotplug(() => controller.manager.burst(), log) : null;
  controller.changed();
  await controller.manager.rescan();

  /** Like hiding the desktop window: nothing runs without a page to show it. */
  function lastClientGone() {
    void controller.act({ type: "pair.cancel" });
    void controller.act({ type: "pair.dismiss" });
    void controller.act({ type: "scan.stop" });
  }

  async function act(req: IncomingMessage, res: ServerResponse) {
    let action: unknown;
    try {
      action = await body(req);
    } catch (error) {
      return reply(res, 400, { error: (error as Error).message });
    }
    if (!isAction(action)) return reply(res, 400, { error: "invalid action" });
    reply(res, 200, await controller.act(action));
  }

  function events(req: IncomingMessage, res: ServerResponse) {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store" });
    // The current state first, so a reconnecting page catches up.
    send(res, controller.state());
    clients.add(res);
    req.on("close", () => {
      clients.delete(res);
      if (!clients.size) lastClientGone();
    });
  }

  return {
    handle(req, res) {
      const path = (req.url ?? "").split("?")[0];
      if (!path?.startsWith("/api/")) return false;
      const problem = refusal(req);
      if (problem) reply(res, 403, { error: problem });
      else if (req.method === "GET" && path === "/api/state") reply(res, 200, controller.state());
      else if (req.method === "GET" && path === "/api/events") events(req, res);
      else if (req.method === "POST" && path === "/api/act") void act(req, res);
      else reply(res, 404, { error: "not found" });
      return true;
    },
    stop: async () => { await Promise.all([stopHotplug?.(), controller.stop()]); },
  };
}
