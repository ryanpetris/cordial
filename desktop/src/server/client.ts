// The window API over the development server's HTTP endpoints; see backend.ts.
import type { ActionResult, AppState, DesktopApi } from "../shared/state.ts";

const listeners = new Set<(state: AppState) => void>();
// Reconnects by itself after a server restart; the server then sends the current state.
new EventSource("/api/events").addEventListener("message", (event: MessageEvent<string>) => {
  const state = JSON.parse(event.data) as AppState;
  for (const listener of listeners) listener(state);
});

async function json<T>(response: Response): Promise<T> {
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return (await response.json()) as T;
}

const api: DesktopApi = {
  host: { desktop: false },
  state: () => fetch("/api/state").then((r) => json<AppState>(r)),
  onState: (listener) => {
    listeners.add(listener);
    return () => void listeners.delete(listener);
  },
  // Only the desktop's tray and notifications navigate.
  onNavigate: () => () => {},
  act: async (action) => {
    try {
      const response = await fetch("/api/act", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(action) });
      return await json<ActionResult>(response);
    } catch (error) {
      return { ok: false, message: `The development server didn't answer: ${(error as Error).message}` };
    }
  },
};

window.cordial = api;
await import("../renderer/main.tsx");
