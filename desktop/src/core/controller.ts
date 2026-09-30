// Combines every adapter into the application state and performs the user's
// actions. Electron-free so it can be tested against a simulated adapter.
import { adapterName } from "../shared/adapter-name.ts";
import {
  AdapterError,
  SessionClosedError,
  type Candidate,
  type Device,
  type HostPlatform,
  type InfoField,
  type Prompt,
  type SettingsSummary,
} from "../protocol/types.ts";
import {
  DEFAULT_PREFERENCES,
  type Action,
  type ActionResult,
  type AdapterEntry,
  type AppState,
  type Battery,
  type DeviceEntry,
  type PairingState,
  type Preferences,
  type ScanState,
  type SettingsResult,
} from "../shared/state.ts";
import { hidppBusy, settingsBusy, settingsLive } from "../shared/settings.ts";
import { DISABLED, clean, codeText, errorText } from "../shared/text.ts";
import { BatteryAlerts, CRITICAL_PERCENT, type Alert } from "./battery.ts";
import { AdapterManager, type ManagerDeps } from "./manager.ts";
import type { AdapterSession } from "./session.ts";

export interface ControllerDeps extends Omit<ManagerDeps, "changed"> {
  preferences: Preferences;
  savePreferences(p: Preferences): void;
  hostPlatform: HostPlatform;
  /** Publishes a new state; called at most every few tens of milliseconds. */
  published(state: AppState): void;
  lowBattery(alert: Alert): void;
  connection(name: string, connected: boolean): void;
}

const PUBLISH_MS = 30;

const message = (error: unknown) =>
  error instanceof AdapterError
    ? errorText(error.wire)
    : error instanceof SessionClosedError
      ? "The adapter disconnected before the change finished."
      : `Couldn't complete that: ${(error as Error).message}`;
const failed = (error: unknown): ActionResult => ({ ok: false, message: message(error) });

function field(fields: InfoField[] | null, key: InfoField["key"]): InfoField | undefined {
  return fields?.find((f) => f.key === key && f.instance === 0 && f.available);
}

export function batteryOf(fields: InfoField[] | null, current = true): Battery | null {
  const percent = field(fields, "battery_percent");
  const charging = field(fields, "battery_charging");
  const b: Battery = {
    percent: percent ? (percent.value as number) : null,
    charging: charging ? (charging.value as boolean) : null,
    percentFresh: current && !!percent?.fresh,
    chargingFresh: current && !!charging?.fresh,
  };
  return b.percent == null && b.charging == null ? null : b;
}

function kindOf(fields: InfoField[] | null, device: Device): DeviceEntry["kind"] {
  const kind = field(fields, "kind")?.value;
  if (kind === "keyboard" || kind === "mouse" || kind === "keyboard_mouse" || kind === "other") return kind;
  const keyboard = device.roles.includes("keyboard");
  const mouse = device.roles.includes("mouse");
  return keyboard && mouse ? "keyboard_mouse" : keyboard ? "keyboard" : mouse ? "mouse" : "other";
}

export class Controller {
  readonly manager: AdapterManager;
  readonly #deps: ControllerDeps;
  #preferences: Preferences;
  readonly #settingsResults = new Map<string, { result: SettingsResult; session: AdapterSession; state: Device["state"] }>();
  #scan: (ScanState & { requestId: number; session: AdapterSession; done: Promise<void> }) | null = null;
  #pairing: (PairingState & { requestId: number; session: AdapterSession; dismissed: boolean; done: Promise<void> }) | null =
    null;
  #watched: string | null = null;
  #timer: ReturnType<typeof setTimeout> | undefined;
  readonly #alerts = new BatteryAlerts();
  #states = new Map<string, Device["state"]>();

  constructor(deps: ControllerDeps) {
    this.#deps = deps;
    this.#preferences = { ...DEFAULT_PREFERENCES, ...deps.preferences };
    this.manager = new AdapterManager({
      ...deps,
      changed: () => this.changed(),
      opened: (id, session) => {
        const key = this.#watched;
        if (key?.startsWith(`${id}/`)) session.watchSettings([key.slice(id.length + 1)]);
      },
    });
  }

  get preferences() {
    return this.#preferences;
  }

  /** Schedules publication of the state after a change. */
  changed() {
    this.#timer ??= setTimeout(() => {
      this.#timer = undefined;
      this.#publish();
    }, PUBLISH_MS);
  }

  #publish() {
    const state = this.state();
    if (this.#preferences.notifyLowBattery)
      for (const alert of this.#alerts.update(state.devices, this.#preferences.lowBatteryPercent))
        this.#deps.lowBattery(alert);
    const states = new Map(state.devices.map((d) => [d.key, d.device.state]));
    if (this.#preferences.notifyConnections)
      for (const d of state.devices) {
        const old = this.#states.get(d.key);
        if (old && old !== d.device.state && (d.device.state === "connected" || old === "connected"))
          this.#deps.connection(d.name, d.device.state === "connected");
      }
    this.#states = states;
    this.#deps.published(state);
  }

  // ---- State -------------------------------------------------------------

  state(): AppState {
    const adapters: AdapterEntry[] = [];
    const devices: DeviceEntry[] = [];
    for (const { id, session } of this.manager.connected.values()) {
      const view = session.view;
      const status = session.status;
      const ready = session.readiness;
      const entries = ready.state === "ready" ? [...view.devices.values()].map((d) => this.#device(id, session, d)) : [];
      const attention: string[] = [];
      if (ready.state === "failed") attention.push(`Not ready: ${errorText(ready.error)}`);
      else if (ready.state === "ready" && !status.radio_ready) attention.push("Bluetooth isn't ready");
      else if (ready.state === "ready" && !status.storage_ready) attention.push("Storage isn't ready");
      const needsPairing = entries.filter((d) => d.device.pairing_state === "needs_pairing").length;
      if (needsPairing) attention.push(`${needsPairing} ${needsPairing === 1 ? "device needs" : "devices need"} pairing again`);
      adapters.push({
        name: view.name ?? status.name,
        id,
        connection: "connected",
        connectError: null,
        readiness: ready.state,
        status,
        capabilities: session.capabilities,
        platform: view.platform,
        attention,
      });
      devices.push(...entries);
    }
    for (const d of this.manager.disconnected.values()) {
      if (!d.path) continue;
      adapters.push({
        name: d.status.name,
        id: d.id,
        connection: d.connecting ? "connecting" : "disconnected",
        connectError: d.error,
        readiness: "waiting",
        status: null,
        capabilities: [],
        platform: null,
        attention: [],
      });
    }
    adapters.sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
    devices.sort((a, b) => a.name.localeCompare(b.name) || a.key.localeCompare(b.key));
    const keys = new Set(devices.map((d) => d.key));
    for (const key of this.#settingsResults.keys()) if (!keys.has(key)) this.#settingsResults.delete(key);
    // Discovery and pairing end with their adapter's session.
    const live = (s: AdapterSession) => [...this.manager.connected.values()].some((c) => c.session === s);
    if (this.#scan && !live(this.#scan.session)) this.#scan = null;
    if (this.#pairing && !live(this.#pairing.session)) this.#pairing = null;
    const pairing = this.#pairing && !this.#pairing.dismissed ? this.#pairing : null;
    return {
      adapters,
      devices,
      scan: this.#scan && {
        adapterId: this.#scan.adapterId,
        running: this.#scan.running,
        candidates: this.#scan.candidates,
        error: this.#scan.error,
      },
      pairing: pairing && {
        adapterId: pairing.adapterId,
        candidateId: pairing.candidateId,
        name: pairing.name,
        phase: pairing.phase,
        prompt: pairing.prompt,
        deviceKey: pairing.deviceKey,
        message: pairing.message,
      },
      preferences: this.#preferences,
      hostPlatform: this.#deps.hostPlatform,
    };
  }

  #device(adapterId: string, session: AdapterSession, device: Device): DeviceEntry {
    const info = session.view.info(device.device_id);
    const key = `${adapterId}/${device.device_id}`;
    // The last valid reported name wins; the pairing name is the fallback.
    const name = clean(session.view.reportedName(device.device_id) ?? "") || clean(device.name ?? "") || "Unnamed device";
    const settings = this.#watched === key ? session.view.settings(device.device_id) : null;
    const outcome = this.#settingsResults.get(key);
    if (outcome && (outcome.session !== session || outcome.state !== device.state)) this.#settingsResults.delete(key);
    return {
      key,
      adapterId,
      device,
      name,
      kind: kindOf(info, device),
      battery: batteryOf(info, session.view.infoCurrent(device.device_id)),
      info,
      infoCurrent: session.view.infoCurrent(device.device_id),
      pending: session.pendingFor(device.device_id),
      infoError: session.view.infoError(device.device_id),
      settings: settings ? { ...settings, result: this.#settingsResults.get(key)?.result ?? null } : null,
    };
  }

  // ---- Actions -----------------------------------------------------------

  #session(adapterId: string): AdapterSession | null {
    return this.manager.connected.get(adapterId)?.session ?? null;
  }

  #target(key: string): { session: AdapterSession; id: string } | null {
    const slash = key.indexOf("/");
    const session = this.#session(key.slice(0, slash));
    const id = key.slice(slash + 1);
    return session && session.view.devices.has(id) ? { session, id } : null;
  }

  async act(action: Action): Promise<ActionResult> {
    const gone: ActionResult = { ok: false, message: "That device or adapter is no longer available." };
    try {
      switch (action.type) {
        case "device.connect.cancel": {
          const t = this.#target(action.key);
          if (!t) return gone;
          const pending = t.session.pendingFor(t.id).find((p) => p.command === "device.connect");
          if (pending) {
            try { await t.session.request("request.cancel", { request_id: pending.id }); }
            catch (error) { if (!(error instanceof AdapterError && error.wire.code === "not_pending")) throw error; }
          }
          return { ok: true };
        }
        case "settings.reload": {
          const t = this.#target(action.key);
          if (!t) return gone;
          t.session.reloadSettings(t.id);
          this.changed();
          return { ok: true };
        }
        case "settings.refresh":
        case "settings.apply": {
          const t = this.#target(action.key);
          if (!t) return gone;
          const entry = this.#device(t.session.adapterId, t.session, t.session.view.devices.get(t.id)!);
          if (settingsBusy(entry)) return { ok: false, message: codeText("busy") };
          if (!settingsLive(entry)) return { ok: false, message: codeText("not_connected") };
          if (action.type === "settings.apply" && !entry.device.hidpp_enabled) return { ok: false, message: codeText("hidpp_disabled") };
          this.#settingsResults.delete(action.key);
          const kind = action.type === "settings.refresh" ? "refresh" : "apply";
          try {
            const counts = await t.session.request(kind === "refresh" ? "hidpp.setting.refresh" : "hidpp.setting.apply", { device_id: t.id }, {
              onChunk: (row) => {
                t.session.view.putSetting(t.id, row.revision, row.setting);
                this.changed();
              },
            });
            this.#settingsResults.set(action.key, { result: { kind, counts, error: null }, session: t.session, state: t.session.view.devices.get(t.id)?.state ?? entry.device.state });
          } catch (error) {
            const details = error instanceof AdapterError && "details" in error.wire ? error.wire.details : null;
            const counts = details && "count" in details ? details as SettingsSummary : null;
            this.#settingsResults.set(action.key, { result: { kind, counts, error: message(error) }, session: t.session, state: t.session.view.devices.get(t.id)?.state ?? entry.device.state });
            return { ok: false, message: message(error), inline: true };
          } finally {
            t.session.mutated();
            this.changed();
          }
          return { ok: true };
        }
        case "device.info.refresh": {
          const t = this.#target(action.key);
          if (!t) return gone;
          await t.session.refreshInfo(t.id);
          return { ok: true };
        }
        case "device.connect":
        case "device.disconnect":
        case "device.unpair": {
          const t = this.#target(action.key);
          if (!t) return gone;
          const args = { device_id: t.id };
          const command = {
            "device.connect": "device.connect",
            "device.disconnect": "device.disconnect",
            "device.unpair": "device.unpair",
          } as const;
          await t.session.request(command[action.type], args);
          t.session.mutated();
          return { ok: true };
        }
        case "device.enabled":
        case "device.trusted":
        case "device.blocked":
        case "device.hidpp": {
          const t = this.#target(action.key);
          if (!t) return gone;
          if (action.type === "device.hidpp" && hidppBusy(this.#device(t.session.adapterId, t.session, t.session.view.devices.get(t.id)!)))
            return { ok: false, message: codeText("busy") };
          if (action.type === "device.enabled")
            await t.session.request("device.enabled.set", { device_id: t.id, enabled: action.value });
          else if (action.type === "device.trusted")
            await t.session.request("device.trusted.set", { device_id: t.id, trusted: action.value });
          else if (action.type === "device.blocked")
            await t.session.request("device.blocked.set", { device_id: t.id, blocked: action.value });
          else await t.session.request("device.hidpp.set", { device_id: t.id, enabled: action.value });
          t.session.mutated();
          return { ok: true };
        }
        case "setting.set":
        case "setting.forget": {
          const t = this.#target(action.key);
          if (!t) return gone;
          const entry = this.#device(t.session.adapterId, t.session, t.session.view.devices.get(t.id)!);
          if (settingsBusy(entry)) return { ok: false, message: codeText("busy") };
          if (action.type === "setting.set" && !settingsLive(entry)) return { ok: false, message: codeText("not_connected") };
          const row =
            action.type === "setting.set"
              ? await t.session.request("hidpp.setting.set", { device_id: t.id, key: action.setting, value: action.value })
              : await t.session.request("hidpp.setting.forget", { device_id: t.id, key: action.setting });
          t.session.view.putSetting(t.id, row.revision, row.setting);
          this.#publish();
          return { ok: true };
        }
        case "settings.watch": {
          this.#watched = action.key;
          for (const { id, session } of this.manager.connected.values()) {
            const t = action.key?.startsWith(`${id}/`) ? action.key.slice(id.length + 1) : null;
            session.watchSettings(t ? [t] : []);
          }
          this.changed();
          return { ok: true };
        }
        case "adapter.name": {
          const session = this.#session(action.adapterId);
          if (!session) return gone;
          const name = action.name === null ? null : adapterName(action.name);
          if (action.name !== null && name === null) return { ok: false, message: "Invalid adapter name" };
          if (!session.status.storage_ready) return { ok: false, message: "Adapter storage is not ready" };
          const result = await session.request("adapter.name.set", { name });
          session.view.setAdapter(result.revision, result.host_platform, result.name);
          session.mutated();
          this.changed();
          return { ok: true };
        }
        case "adapter.platform": {
          const session = this.#session(action.adapterId);
          if (!session) return gone;
          if (!session.status.storage_ready) return { ok: false, message: "Adapter storage is not ready" };
          const result = await session.request("adapter.platform.set", { platform: action.platform });
          session.view.setAdapter(result.revision, result.host_platform, result.name);
          session.mutated();
          this.changed();
          return { ok: true };
        }
        case "adapter.disconnect":
          await this.manager.disconnect(action.adapterId);
          this.changed();
          return { ok: true };
        case "adapter.connect": {
          const error = await this.manager.connect(action.adapterId);
          return error ? { ok: false, message: error } : { ok: true };
        }
        case "adapters.refresh":
          this.manager.burst();
          return { ok: true };
        case "scan.start":
          return this.#startScan(action.adapterId);
        case "scan.stop":
          await this.#stopScan();
          return { ok: true };
        case "pair.start":
          return this.#pair(action.adapterId, action.candidateId);
        case "pair.reply":
          return this.#reply(action.accept, action.value);
        case "pair.cancel":
          if (this.#pairing?.phase === "pairing")
            await this.#pairing.session.request("request.cancel", { request_id: this.#pairing.requestId });
          return { ok: true };
        case "pair.dismiss":
          // A pairing still finishing is forgotten when it ends; a connect
          // continues in the background and shows on the device page.
          if (this.#pairing) {
            if (this.#pairing.phase === "pairing" || this.#pairing.phase === "connecting") this.#pairing.dismissed = true;
            else this.#pairing = null;
          }
          this.changed();
          return { ok: true };
        case "preferences": {
          const p = { ...this.#preferences, ...action.preferences };
          p.lowBatteryPercent = Math.min(50, Math.max(CRITICAL_PERCENT, Math.round(p.lowBatteryPercent)));
          this.#setPreferences(p);
          return { ok: true };
        }
        case "adapter.menu":
        case "app.menu":
          return { ok: true };
      }
    } catch (error) {
      return failed(error);
    }
  }

  #setPreferences(p: Preferences) {
    this.#preferences = p;
    this.#deps.savePreferences(p);
    this.changed();
  }

  // ---- Discovery and pairing ----------------------------------------------

  async #startScan(adapterId: string): Promise<ActionResult> {
    await this.#stopScan();
    const session = this.#session(adapterId);
    if (!session) return { ok: false, message: "That adapter is no longer available." };
    const classic = session.capabilities.includes("classic");
    const ble = session.capabilities.includes("ble");
    if (!classic && !ble) return { ok: false, message: "This adapter can't search for devices." };
    const candidates: Candidate[] = [];
    const started = session.start(
      "discovery.scan",
      { transport: classic && ble ? "both" : classic ? "classic" : "ble", duration_ms: 0 },
      {
        onEvent: (event) => {
          if (event.event !== "discovery.result") return;
          const i = candidates.findIndex((c) => c.candidate_id === event.data.candidate_id);
          if (i === -1) candidates.push(event.data);
          else candidates[i] = event.data;
          this.changed();
        },
      },
    );
    const done = started.result
      .then(
        () => {},
        (error: unknown) => {
          if (this.#scan === scan && error instanceof AdapterError && error.wire.code !== "cancelled")
            scan.error = errorText(error.wire);
        },
      )
      .finally(() => {
        scan.running = false;
        this.changed();
      });
    const scan = { adapterId, running: true, candidates, error: null as string | null, requestId: started.id, session, done };
    this.#scan = scan;
    this.changed();
    return { ok: true };
  }

  async #stopScan() {
    const scan = this.#scan;
    if (!scan) return;
    this.#scan = null;
    this.changed();
    if (!scan.running) return;
    await scan.session.request("request.cancel", { request_id: scan.requestId }).catch(() => {});
    // The adapter accepts another scan only after this one has ended.
    await Promise.race([scan.done, new Promise((r) => setTimeout(r, 2000))]);
  }

  async #pair(adapterId: string, candidateId: string): Promise<ActionResult> {
    const session = this.#session(adapterId);
    if (!session) return { ok: false, message: "That adapter is no longer available." };
    if (this.#pairing && !this.#pairing.dismissed && (this.#pairing.phase === "pairing" || this.#pairing.phase === "connecting"))
      return { ok: false, message: "Another device is being added." };
    // A dismissed pairing may still be tearing down (up to 15 seconds).
    if (this.#pairing?.dismissed && this.#pairing.phase === "pairing") await Promise.race([this.#pairing.done, new Promise((r) => setTimeout(r, 16000))]);
    const candidate = this.#scan?.candidates.find((c) => c.candidate_id === candidateId);
    const name = clean(candidate?.name ?? "") || "the device";
    // Candidates stay usable after their scan is cancelled.
    const scan = this.#scan;
    if (scan?.running) {
      await scan.session.request("request.cancel", { request_id: scan.requestId }).catch(() => {});
      await Promise.race([scan.done, new Promise((r) => setTimeout(r, 2000))]);
    }
    const started = session.start(
      "pairing.start",
      { candidate_id: candidateId },
      {
        onEvent: (event) => {
          if (!this.#pairing || this.#pairing.requestId !== started.id) return;
          if (event.event === "pairing.prompt" || event.event === "pairing.display") {
            const prompt: Prompt = event.data;
            this.#pairing.prompt = {
              kind: event.event === "pairing.prompt" ? "prompt" : "display",
              prompt,
              expiresAt: Date.now() + prompt.expires_in_ms,
            };
            this.changed();
          }
        },
      },
    );
    const pairing = {
      adapterId,
      candidateId,
      name,
      phase: "pairing" as PairingState["phase"],
      prompt: null,
      deviceKey: null as string | null,
      message: null as string | null,
      requestId: started.id,
      session,
      dismissed: false,
      done: Promise.resolve(),
    };
    this.#pairing = pairing;
    this.changed();
    pairing.done = (async () => {
      try {
        const { device } = await started.result;
        pairing.prompt = null;
        pairing.deviceKey = `${adapterId}/${device.device_id}`;
        pairing.name = clean(device.name ?? "") || name;
        session.mutated();
        if (!device.effective_enabled) {
          pairing.phase = "saved";
          pairing.message =
            device.enabled_reason === "disabled" || !device.enabled_reason
              ? "It was saved but is turned off; turn on “Use This Device” to connect it."
              : `It was saved but can't connect yet. ${DISABLED[device.enabled_reason]}`;
          return;
        }
        pairing.phase = "connecting";
        this.changed();
        try {
          await session.request("device.connect", { device_id: device.device_id });
          pairing.phase = "connected";
        } catch (error) {
          pairing.phase = "saved";
          pairing.message = `It was saved, but connecting failed. ${message(error)}`;
        }
      } catch (error) {
        pairing.prompt = null;
        pairing.phase = error instanceof AdapterError && error.wire.code === "cancelled" ? "cancelled" : "failed";
        pairing.message =
          error instanceof AdapterError && error.wire.code === "cancelled"
            ? "Adding the device was cancelled."
            : message(error);
      } finally {
        if (pairing.dismissed && this.#pairing === pairing) this.#pairing = null;
        this.changed();
      }
    })();
    return { ok: true };
  }

  async #reply(accept: boolean, value?: string): Promise<ActionResult> {
    const pairing = this.#pairing;
    const prompt = pairing?.prompt;
    if (!pairing || !prompt || prompt.kind !== "prompt") return { ok: false, message: "No pairing prompt is waiting." };
    const entry = prompt.prompt.method === "enter_passkey" || prompt.prompt.method === "enter_pin";
    try {
      await pairing.session.request("pairing.reply", {
        request_id: pairing.requestId,
        prompt_id: prompt.prompt.prompt_id,
        action: accept ? "accept" : "reject",
        ...(accept && entry ? { value: value ?? "" } : {}),
      });
      if (pairing.prompt === prompt) pairing.prompt = null;
      this.changed();
      return { ok: true };
    } catch (error) {
      if (error instanceof AdapterError && error.wire.code === "stale_prompt") {
        if (pairing.prompt === prompt) pairing.prompt = null;
        this.changed();
        return { ok: false, message: codeText("stale_prompt") };
      }
      return failed(error);
    }
  }

  async stop() {
    await this.manager.stop();
  }
}
