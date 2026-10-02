// The controller in the page, for the web build: adapters through Web Serial,
// or simulated ones for a demo. Preferences stay in this browser.
import icon from "../../assets/icons/app.png";
import { Controller } from "../core/controller.ts";
import { PORT_FILTERS, listPorts, openWebSerial } from "@cordial/client/web";
import { preferencesFrom, type AppState, type DesktopApi, type HostPlatform, type Preferences } from "../shared/state.ts";

const PREFERENCES_KEY = "cordial.preferences";
const log = (message: string) => console.log(`[cordial] ${message}`);

function loadPreferences(): Preferences {
  try {
    return preferencesFrom(JSON.parse(localStorage.getItem(PREFERENCES_KEY) ?? "null"));
  } catch {
    return preferencesFrom(null);
  }
}

function savePreferences(p: Preferences) {
  try {
    localStorage.setItem(PREFERENCES_KEY, JSON.stringify(p));
  } catch (error) {
    log(`saving preferences failed: ${(error as Error).message}`);
  }
}

function hostPlatform(): HostPlatform {
  const platform = (navigator.userAgentData?.platform ?? navigator.platform).toLowerCase();
  return platform.startsWith("win") ? "windows" : platform.startsWith("mac") ? "mac" : "linux";
}

const notifying = (p: Preferences) => p.notifyLowBattery || p.notifyConnections;

/** Asks for notification permission; call from a user action. */
function allowNotifications() {
  if (typeof Notification !== "undefined" && Notification.permission === "default") void Notification.requestPermission();
}

function notify(title: string, body: string) {
  if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
  new Notification(title, { body, icon }).addEventListener("click", () => window.focus());
}

/** Starts the controller; `simulate` adapters replace Web Serial when above zero. */
export async function startWebBackend(simulate: number): Promise<DesktopApi> {
  const serial = navigator.serial;
  const ports =
    simulate > 0 || !serial
      ? (await import("../fake/ports.ts")).simulatedPorts(simulate)
      : { listPorts: () => listPorts(serial), openTransport: openWebSerial };
  const listeners = new Set<(state: AppState) => void>();
  const controller = new Controller({
    ...ports,
    log,
    preferences: loadPreferences(),
    savePreferences,
    hostPlatform: hostPlatform(),
    published: (state) => {
      for (const listener of listeners) listener(state);
    },
    lowBattery: (alert) =>
      notify(alert.level === "critical" ? `${alert.name}'s battery is critically low.` : `${alert.name} has a low battery.`, `The battery is at ${alert.percent}%. Charge it soon.`),
    connection: (name, connected) => notify(name, connected ? "Connected" : "Disconnected"),
  });
  if (serial && !simulate) {
    serial.addEventListener("connect", () => controller.manager.burst());
    serial.addEventListener("disconnect", () => controller.manager.burst());
  }
  // Closing the port ends the adapter's session; the browser closes it anyway
  // when the page goes. A page kept for back and forward navigation keeps
  // running when shown again.
  window.addEventListener("pagehide", (event) => {
    if (!event.persisted) void controller.stop();
  });
  controller.changed();
  await controller.manager.rescan();

  return {
    host: {
      desktop: false,
      choosePort:
        serial && !simulate
          ? async () => {
              await serial.requestPort({ filters: PORT_FILTERS });
              if (notifying(controller.preferences)) allowNotifications();
              await controller.manager.rescan();
            }
          : undefined,
    },
    state: async () => controller.state(),
    onState: (listener) => {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
    // Only the desktop's tray and notifications navigate.
    onNavigate: () => () => {},
    act: (action) => {
      // Still inside the click that turned a notification on.
      if (action.type === "preferences" && notifying({ ...controller.preferences, ...action.preferences })) allowNotifications();
      return controller.act(action);
    },
  };
}
