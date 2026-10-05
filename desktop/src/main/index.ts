declare const __CORDIAL_VERSION__: string;
// Electron entry point: window, tray, menus, notifications and hotplug.
import {
  BrowserWindow,
  Menu,
  Tray,
  app,
  ipcMain,
  nativeImage,
  nativeTheme,
  session,
  type MenuItemConstructorOptions,
} from "electron";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { isAction } from "../core/actions.ts";
import { Controller } from "../core/controller.ts";
import { listPorts, openSerial } from "@cordial/client/node";
import { hostPlatform, watchHotplug } from "../node/host.ts";
import { preferencesFrom, type Action, type AppState, type Navigation, type Preferences } from "../shared/state.ts";
import { profileAlertText } from "../shared/text.ts";
import { trayModel, type TrayModel } from "./tray-model.ts";
import { closeNotifications, showNotification } from "./notifications.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const icons = join(root, "assets", "icons");
const hidden = process.argv.includes("--hidden");
const log = (message: string) => console.log(`[cordial] ${new Date().toISOString()} ${message}`);

if (!app.requestSingleInstanceLock()) {
  app.exit(0);
}

let window: BrowserWindow | null = null;
let tray: Tray | null = null;
let trayKey = "";
let quitting = false;
let controller: Controller;
let stopHotplug: (() => Promise<void>) | null = null;
let pendingNavigation: Navigation | null = null;
/** The device whose settings the window shows, kept while it is hidden. */

// ---- Preferences ----------------------------------------------------------

const preferencesPath = () => join(app.getPath("userData"), "preferences.json");

function loadPreferences(): Preferences {
  try {
    return preferencesFrom(JSON.parse(readFileSync(preferencesPath(), "utf8")));
  } catch {
    return preferencesFrom(null);
  }
}

let savedAutostart: boolean | null = null;
function savePreferences(p: Preferences) {
  try {
    mkdirSync(app.getPath("userData"), { recursive: true });
    writeFileSync(preferencesPath(), JSON.stringify(p, null, 2));
  } catch (error) {
    log(`saving preferences failed: ${(error as Error).message}`);
  }
  if (p.startAtLogin !== savedAutostart) setAutostart(p.startAtLogin);
}

/** XDG autostart entry that starts the app in the tray. */
function setAutostart(enabled: boolean) {
  savedAutostart = enabled;
  if (process.platform !== "linux") {
    app.setLoginItemSettings({ openAtLogin: enabled, args: ["--hidden"] });
    return;
  }
  const file = join(process.env.XDG_CONFIG_HOME || join(homedir(), ".config"), "autostart", "cordial-desktop.desktop");
  try {
    if (!enabled) return rmSync(file, { force: true });
    const exec = process.env.APPIMAGE ?? process.execPath;
    const args = app.isPackaged ? [] : [app.getAppPath()];
    // Desktop Entry quoting escapes " ` $ \ with a backslash inside double
    // quotes; the value's own string escaping then doubles each backslash,
    // and % must be written %%.
    const quote = (s: string) =>
      `"${s.replace(/(["`$\\])/g, "\\$1")}"`.replace(/\\/g, "\\\\").replace(/%/g, "%%");
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(
      file,
      `[Desktop Entry]\nType=Application\nName=Cordial\nComment=Manage Cordial Bluetooth adapters\nExec=${[exec, ...args].map(quote).join(" ")} --hidden\nIcon=cordial-desktop\nX-GNOME-Autostart-enabled=true\nTerminal=false\n`,
    );
  } catch (error) {
    log(`updating autostart failed: ${(error as Error).message}`);
  }
}

// ---- Window ----------------------------------------------------------------

function showWindow(to?: Navigation) {
  if (to) pendingNavigation = to;
  if (!window) createWindow();
  else {
    if (window.isMinimized()) window.restore();
    window.show();
    window.focus();
    if (to) window.webContents.send("navigate", to);
  }
}

const HEADER_HEIGHT = 46;

/** Window controls drawn over the top right of the window, in its colors, level with the page header. */
function titleBarOverlay() {
  const dark = nativeTheme.shouldUseDarkColors;
  return { color: dark ? "#222226" : "#fafafb", symbolColor: dark ? "#ffffff" : "#1e1e22", height: HEADER_HEIGHT };
}

/** Shortcuts, since the integrated header has no menu bar to hold them. */
function shortcut(input: Electron.Input): (() => void) | null {
  if (input.type !== "keyDown") return null;
  const ctrl = input.control || input.meta;
  const key = input.key.toLowerCase();
  if (key === "f5" || (ctrl && key === "r")) return () => void act({ type: "adapters.refresh" });
  if (ctrl && key === "n") return () => showWindow({ page: "add-device" });
  if (ctrl && key === ",") return () => showWindow({ page: "preferences" });
  if (ctrl && key === "w") return () => window?.close();
  if (ctrl && key === "q") return () => void quit();
  if (!app.isPackaged && ctrl && input.shift && key === "i") return () => window?.webContents.toggleDevTools();
  return null;
}

function createWindow() {
  window = new BrowserWindow({
    width: 980,
    height: 680,
    minWidth: 720,
    minHeight: 480,
    show: false,
    title: "Cordial",
    icon: join(icons, "app.png"),
    titleBarStyle: "hidden",
    titleBarOverlay: titleBarOverlay(),
    backgroundColor: nativeTheme.shouldUseDarkColors ? "#242424" : "#fafafa",
    webPreferences: {
      preload: join(root, "out", "preload", "index.cjs"),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
    },
  });
  window.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
  window.webContents.on("will-attach-webview", (event) => event.preventDefault());
  window.webContents.on("before-input-event", (event, input) => {
    const run = shortcut(input);
    if (run) {
      event.preventDefault();
      run();
    }
  });
  window.webContents.on("will-navigate", (event) => event.preventDefault());
  window.on("close", (event) => {
    if (!quitting) {
      event.preventDefault();
      window?.hide();
      // Nothing runs for a hidden window: stop discovery and uncommitted
      // pairing, and close the window's dialogs.
      void controller.act({ type: "pair.cancel" });
      void controller.act({ type: "pair.dismiss" });
      void controller.act({ type: "scan.stop" });
      window?.webContents.send("navigate", { page: "hidden" } satisfies Navigation);
    }
  });
  window.on("closed", () => (window = null));
  window.once("ready-to-show", () => window?.show());
  window.webContents.on("did-finish-load", () => {
    if (pendingNavigation) window?.webContents.send("navigate", pendingNavigation);
    pendingNavigation = null;
  });
  void window.loadFile(join(root, "out", "renderer", "index.html"));
}

// ---- Tray ------------------------------------------------------------------

function trayVariant(): "light" | "dark" {
  // GNOME's top bar is dark in both styles.
  if (/GNOME/i.test(process.env.XDG_CURRENT_DESKTOP ?? "")) return "light";
  return nativeTheme.shouldUseDarkColors ? "light" : "dark";
}

/** Menu labels treat & as a mnemonic marker. */
const label = (s: string) => s.replace(/&/g, "&&");

function trayMenu(model: TrayModel, state: AppState): Menu {
  const items: MenuItemConstructorOptions[] = [];
  for (const reason of model.attention) items.push({ label: label(reason), enabled: false });
  if (model.attention.length) items.push({ type: "separator" });
  const anyDevices = model.groups.some((g) => g.devices.length);
  for (const group of model.groups) {
    if (group.title) items.push({ label: label(group.title), enabled: false });
    for (const d of group.devices)
      items.push({
        label: label(d.label),
        submenu: [
          d.connected
            ? { label: "Disconnect", click: () => void act({ type: "device.disconnect", key: d.key }) }
            : { label: "Connect", enabled: d.canConnect, click: () => void act({ type: "device.connect", key: d.key }) },
          { label: "Open", click: () => showWindow({ page: "device", key: d.key }) },
        ],
      });
  }
  if (!anyDevices) items.push({ label: state.adapters.length ? "No Saved Devices" : "No Adapter Connected", enabled: false });
  items.push(
    { type: "separator" },
    {
      label: "Add Device",
      enabled: state.adapters.some((a) => a.connection === "connected" && a.readiness === "ready"),
      click: () => showWindow({ page: "add-device" }),
    },
    { label: "Open Cordial", click: () => showWindow() },
    { type: "separator" },
    { label: "Settings", click: () => showWindow({ page: "preferences" }) },
    { label: "Quit", click: () => quit() },
  );
  return Menu.buildFromTemplate(items);
}

function updateTray(state: AppState) {
  const model = trayModel(state);
  if (!model.visible) {
    tray?.destroy();
    tray = null;
    trayKey = "";
    return;
  }
  const icon = join(icons, `tray-${model.base}-${model.badge}-${trayVariant()}.png`);
  const key = JSON.stringify([model, icon, state.adapters.map((a) => a.readiness)]);
  if (tray && key === trayKey) return;
  trayKey = key;
  if (!tray) {
    tray = new Tray(nativeImage.createFromPath(icon));
    tray.on("click", () => showWindow());
  } else tray.setImage(nativeImage.createFromPath(icon));
  tray.setToolTip(model.tooltip);
  tray.setContextMenu(trayMenu(model, state));
}

// ---- Notifications ---------------------------------------------------------

/** Linux notification servers may interpret markup in bodies, not titles. */
const plain = (s: string) =>
  process.platform === "linux" ? s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;") : s;

function notify(title: string, body: string | null, to?: Navigation) {
  showNotification(
    { title, ...(body === null ? {} : { body: plain(body) }), icon: nativeImage.createFromPath(join(icons, "app.png")) },
    to ? () => showWindow(to) : undefined,
  );
}

// ---- Actions ---------------------------------------------------------------

async function act(action: Action) {
  const result = await controller.request(action);
  if (!result.ok && !window?.isVisible()) notify("Cordial", result.message);
  return result;
}

function adapterMenu(adapterId: string) {
  const adapter = controller.state().adapters.find((a) => a.id === adapterId);
  if (!adapter || !window) return;
  const connected = adapter.connection === "connected";
  Menu.buildFromTemplate([
    connected
      ? { label: "Disconnect", click: () => void act({ type: "adapter.disconnect", adapterId }) }
      : {
          label: "Connect",
          enabled: adapter.connection === "disconnected",
          click: () => void act({ type: "adapter.connect", adapterId }),
        },
    { label: "Rename", enabled: connected && !!adapter.status?.ready, click: () => showWindow({ page: "adapter", id: adapterId, rename: true }) },
  ]).popup({ window });
}

function appMenu(x: number, y: number) {
  if (!window) return;
  Menu.buildFromTemplate([
    { label: "Refresh Adapters", accelerator: "F5", registerAccelerator: false, click: () => void act({ type: "adapters.refresh" }) },
    { type: "separator" },
    { label: "Settings", accelerator: "CmdOrCtrl+,", registerAccelerator: false, click: () => showWindow({ page: "preferences" }) },
    { label: "Quit", accelerator: "CmdOrCtrl+Q", registerAccelerator: false, click: () => void quit() },
  ]).popup({ window, x: Math.round(x), y: Math.round(y) });
}

// ---- Lifecycle -------------------------------------------------------------

async function quit() {
  if (quitting) return;
  quitting = true;
  closeNotifications();
  await Promise.race([Promise.all([stopHotplug?.(), controller.stop()]), new Promise((r) => setTimeout(r, 1500))]);
  tray?.destroy();
  app.exit(0);
}

app.on("second-instance", () => showWindow());
app.on("window-all-closed", () => {
  // Keep running in the tray.
});
app.on("before-quit", (event) => {
  if (!quitting) {
    event.preventDefault();
    void quit();
  }
});

void app.whenReady().then(async () => {
  app.setName("Cordial");
  app.setAboutPanelOptions({ applicationName: "Cordial", applicationVersion: __CORDIAL_VERSION__ });
  // The window needs no browser permissions.
  session.defaultSession.setPermissionRequestHandler((_, __, callback) => callback(false));
  session.defaultSession.setPermissionCheckHandler(() => false);
  if (process.platform === "linux") app.setDesktopName("cordial-desktop.desktop");
  const preferences = loadPreferences();
  savedAutostart = preferences.startAtLogin;
  const fake = Number(process.env.CORDIAL_DESKTOP_SIMULATE ?? 0);
  const simulated = fake > 0 ? (await import("../fake/ports.ts")).simulatedPorts(fake) : null;
  const ports = simulated ?? { listPorts, openTransport: openSerial };
  controller = new Controller({
    ...ports,
    log,
    preferences,
    savePreferences,
    hostPlatform: hostPlatform(),
    published: (state) => {
      updateTray(state);
      window?.webContents.send("state", state);
    },
    lowBattery: (alert) =>
      notify(
        alert.level === "critical" ? `${alert.name}'s battery is critically low.` : `${alert.name} has a low battery.`,
        `The battery is at ${alert.percent}%. Charge it soon.`,
        { page: "device", key: alert.key },
      ),
    connection: (name, connected) => notify(name, connected ? "Connected" : "Disconnected"),
    profileAlert: (alert) => {
      const { title, body } = profileAlertText(alert);
      notify(title, body, alert.kind === "memory" ? { page: "adapter", id: alert.adapterId } : { page: "device", key: alert.key });
    },
  });

  ipcMain.handle("state", (event) => {
    if (event.sender !== window?.webContents) throw new Error("unexpected sender");
    return controller.state();
  });
  ipcMain.handle("act", (event, action: unknown) => {
    if (event.sender !== window?.webContents) throw new Error("unexpected sender");
    if (!isAction(action)) throw new Error("invalid action");
    if (action.type === "adapter.menu") {
      adapterMenu(action.adapterId);
      return { ok: true };
    }
    if (action.type === "app.menu") {
      appMenu(action.x, action.y);
      return { ok: true };
    }
    return act(action);
  });
  nativeTheme.on("updated", () => {
    window?.setTitleBarOverlay(titleBarOverlay());
    trayKey = "";
    updateTray(controller.state());
  });

  Menu.setApplicationMenu(null);
  if (simulated) simulated.onHotplug(() => controller.manager.burst());
  else stopHotplug = await watchHotplug(() => controller.manager.burst(), log);
  controller.changed();
  await controller.manager.rescan();
  if (!hidden) showWindow();
});
