// Exposes the narrow window API; the renderer has no Node or Electron access.
import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import type { Action, AppState, DesktopApi, Navigation } from "../shared/state.ts";

function subscribe<T>(channel: string, listener: (value: T) => void) {
  const handler = (_: IpcRendererEvent, value: T) => listener(value);
  ipcRenderer.on(channel, handler);
  return () => void ipcRenderer.removeListener(channel, handler);
}

const api: DesktopApi = {
  host: { desktop: true },
  state: () => ipcRenderer.invoke("state") as Promise<AppState>,
  onState: (listener) => subscribe<AppState>("state", listener),
  onNavigate: (listener) => subscribe<Navigation>("navigate", listener),
  act: (action: Action) => ipcRenderer.invoke("act", action),
};

contextBridge.exposeInMainWorld("cordial", api);
