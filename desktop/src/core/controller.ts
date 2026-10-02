// Combines every adapter into the application state and performs the user's
// actions. Electron-free so it can be tested against a simulated adapter.
// Refusals the adapter would make that this app can predict from what it
// knows are made here, before sending; one the adapter makes anyway is shown
// as it is, without retrying or reading anything again.
import { CordialError } from "@cordial/client";
import { IntegrationKind, Platform, Transport, type Event, type Request, type Response } from "@cordial/protocol";
import { adapterName } from "../shared/adapter-name.ts";
import {
  DEFAULT_PREFERENCES,
  type Action,
  type ActionResult,
  type AdapterEntry,
  type AppState,
  type Battery,
  type DeviceEntry,
  type DeviceRecord,
  type HostPlatform,
  type InfoEntry,
  type PairingState,
  type Preferences,
  type ScanState,
  type SettingsChange,
  type SettingsSave,
  type TransportName,
} from "../shared/state.ts";
import { enabledFull } from "../shared/capacity.ts";
import { settingsBusy } from "../shared/settings.ts";
import { TRANSPORTS, clean, codeText, errorText, inactiveText, infoOf, storageFull, transportDisabledText } from "../shared/text.ts";
import { BatteryAlerts, CRITICAL_PERCENT, type Alert } from "./battery.ts";
import * as convert from "./convert.ts";
import { AdapterManager, type ManagerDeps } from "./manager.ts";
import { failure, type AdapterSession } from "./session.ts";

export interface ControllerDeps extends Omit<ManagerDeps, "changed" | "event"> {
  preferences: Preferences;
  savePreferences(p: Preferences): void;
  hostPlatform: HostPlatform;
  /** Publishes a new state; called at most every few tens of milliseconds. */
  published(state: AppState): void;
  lowBattery(alert: Alert): void;
  connection(name: string, connected: boolean): void;
}

const PUBLISH_MS = 30;
/** How long a scan runs before the user searches again. */
const SCAN_SECONDS = 30;

const failed = (error: unknown): ActionResult => ({ ok: false, message: failure(error) });

const unsupported = (error: unknown) => error instanceof CordialError && convert.errorCode(error.code) === "unsupported";

/** Whether the adapter supports `t` and has it disabled. */
const disabled = (session: AdapterSession, t: TransportName | null) => session.status.transports.some((x) => x.transport === t && !x.enabled);

/** A failure of work on `transports`, naming the transport when the adapter refused it because it
 * now has the transport disabled. */
function transportFailure(error: unknown, session: AdapterSession, transports: (TransportName | null)[]) {
  const off = unsupported(error) ? transports.find((t) => disabled(session, t)) : undefined;
  return off ? transportDisabledText(off) : failure(error);
}

const GONE: ActionResult = { ok: false, message: "This device or adapter is no longer available." };

export function batteryOf(info: InfoEntry[], current = true): Battery | null {
  const percent = infoOf(info, "battery.level");
  const charging = infoOf(info, "battery.charging");
  const b: Battery = {
    percent: typeof percent === "number" ? percent : null,
    charging: typeof charging === "boolean" ? charging : null,
    percentFresh: current && typeof percent === "number",
    chargingFresh: current && typeof charging === "boolean",
  };
  return b.percent == null && b.charging == null ? null : b;
}

function kindOf(device: DeviceRecord): DeviceEntry["kind"] {
  if (device.kind !== "unknown") return device.kind;
  const keyboard = device.roles.includes("keyboard");
  const mouse = device.roles.includes("mouse");
  return keyboard && mouse ? "keyboard_mouse" : keyboard ? "keyboard" : mouse ? "mouse" : "other";
}

type Scan = ScanState & { session: AdapterSession };
type Pairing = PairingState & { session: AdapterSession; dismissed: boolean; transport: TransportName | null };

export class Controller {
  readonly manager: AdapterManager;
  readonly #deps: ControllerDeps;
  #preferences: Preferences;
  readonly #settingsSaves = new Map<string, { save: SettingsSave; session: AdapterSession }>();
  #scan: Scan | null = null;
  /** Scans sent and not yet answered, in the order sent. Each replaces #scan when the adapter
   * accepts it, unless it was stopped meanwhile. */
  #starts: { scan: Scan; stopped: boolean }[] = [];
  #pairing: Pairing | null = null;
  #timer: ReturnType<typeof setTimeout> | undefined;
  readonly #alerts = new BatteryAlerts();
  #states = new Map<string, DeviceRecord["state"]>();

  constructor(deps: ControllerDeps) {
    this.#deps = deps;
    this.#preferences = { ...DEFAULT_PREFERENCES, ...deps.preferences };
    this.manager = new AdapterManager({
      ...deps,
      changed: () => this.changed(),
      event: (session, event) => this.#event(session, event),
      answered: (session, request, response) => this.#answered(session, request, response),
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
      const status = session.status;
      const entries = session.listed ? [...session.devices.values()].map((d) => this.#device(id, session, d)) : [];
      const attention: string[] = [];
      if (status.ready && storageFull(status))
        attention.push("The adapter's storage is full. Remove an unused device or forget a saved setting to pair another device.");
      adapters.push({
        name: status.name,
        id,
        connection: "connected",
        connectError: null,
        readiness: status.ready ? "ready" : "waiting",
        status,
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
        attention: [],
      });
    }
    adapters.sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
    devices.sort((a, b) => a.name.localeCompare(b.name) || a.key.localeCompare(b.key));
    const keys = new Set(devices.map((d) => d.key));
    for (const [key, saved] of this.#settingsSaves)
      if (!keys.has(key) || this.#target(key)?.session !== saved.session) this.#settingsSaves.delete(key);
    // Discovery and pairing end with their adapter's session.
    const live = (s: AdapterSession) => [...this.manager.connected.values()].some((c) => c.session === s);
    if (this.#scan && !live(this.#scan.session)) this.#scan = null;
    // A closed session never answers its pending scans.
    this.#starts = this.#starts.filter((s) => live(s.scan.session));
    if (this.#pairing && !live(this.#pairing.session)) this.#pairing = null;
    this.#follow();
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

  #device(adapterId: string, session: AdapterSession, device: DeviceRecord): DeviceEntry {
    const key = `${adapterId}/${device.id}`;
    const save = this.#settingsSaves.get(key);
    return {
      key,
      adapterId,
      device,
      name: clean(device.name) || "Unnamed device",
      kind: kindOf(device),
      battery: batteryOf(device.info, device.state === "connected"),
      pending: session.pendingFor(device.id),
      warnings: session.warnings.get(device.id) ?? null,
      warningsError: session.warningsErrors.get(device.id) ?? null,
      settings: session.settings.get(device.id) ?? null,
      settingsError: session.settingsErrors.get(device.id) ?? null,
      settingsSave: save?.session === session ? save.save : null,
    };
  }

  /** Follows a saved pairing's device until it connects or can't. */
  #follow() {
    const p = this.#pairing;
    if (!p || p.phase !== "connecting" || !p.deviceKey) return;
    const d = p.session.devices.get(p.deviceKey.slice(p.adapterId.length + 1));
    if (!d) return;
    p.name = clean(d.name) || p.name;
    if (d.state === "connected") p.phase = "connected";
    else if (d.inactive !== null) {
      p.phase = "saved";
      p.message = d.inactive === "disabled"
        ? "The device is paired but turned off in Cordial. Turn on “Use This Device” to connect it."
        : `The device is paired but can't connect. ${inactiveText(d)}`;
    } else if (d.state === "disconnected" && d.error) {
      p.phase = "saved";
      p.message = `The device is paired, but the adapter couldn't connect to it. ${codeText(d.error)}`;
    }
    if (p.phase !== "connecting" && p.dismissed) this.#pairing = null;
  }

  // ---- Actions -----------------------------------------------------------

  #session(adapterId: string): AdapterSession | null {
    return this.manager.connected.get(adapterId)?.session ?? null;
  }

  #target(key: string): { session: AdapterSession; id: string; device: DeviceRecord } | null {
    const slash = key.indexOf("/");
    const session = this.#session(key.slice(0, slash));
    const id = key.slice(slash + 1);
    const device = session?.devices.get(id);
    return session && device ? { session, id, device } : null;
  }

  async #saveSettings(key: string, changes: SettingsChange[]): Promise<ActionResult> {
    const target = this.#target(key);
    if (!target) return GONE;
    const { session, id } = target;
    const entry = this.#device(session.adapterId, session, target.device);
    if (settingsBusy(entry)) return { ok: false, message: codeText("busy") };
    if (!changes.length || new Set(changes.map((c) => c.setting)).size !== changes.length)
      return { ok: false, message: "There are no distinct settings changes to save." };
    const known = new Map((entry.settings ?? []).map((s) => [s.key, s]));
    if (changes.some((c) => !known.has(c.setting))) return { ok: false, message: codeText("not_found") };
    const items: SettingsSave["items"] = changes.map((change) => ({ change, status: "pending", error: null }));
    const save: SettingsSave = { running: true, items };
    this.#settingsSaves.set(key, { save, session });
    this.#publish();
    // Values are saved in one write and forgetting in another, each all or nothing.
    const groups = [items.filter((i) => i.change.type === "set"), items.filter((i) => i.change.type === "forget")];
    let stopped: string | null = null;
    try {
      for (const group of groups) {
        if (!group.length) continue;
        if (stopped) {
          for (const item of group) Object.assign(item, { status: "not_sent", error: stopped });
          continue;
        }
        for (const item of group) item.status = "saving";
        this.#publish();
        try {
          await session.perform(id, "settings", (c) => {
            const refs = group.map((i) => ({ integration: known.get(i.change.setting)!.integration, key: i.change.setting }));
            return group[0]!.change.type === "set"
              ? c.setSettings({
                  device: id,
                  changes: group.map((i, n) => ({ ...refs[n]!, value: convert.value(known.get(i.change.setting)!.type, (i.change as Extract<SettingsChange, { type: "set" }>).value) })),
                })
              : c.forgetSettings({ device: id, settings: refs });
          });
          for (const item of group) item.status = "saved";
        } catch (error) {
          stopped = failure(error);
          for (const item of group) Object.assign(item, { status: "not_saved", error: stopped });
        }
      }
    } finally {
      save.running = false;
      this.#publish();
    }
    const incomplete = save.items.find((i) => i.status !== "saved");
    return incomplete
      ? { ok: false, message: incomplete.error ?? "The adapter couldn't save some settings.", inline: true, settingsSave: save }
      : { ok: true, settingsSave: save };
  }

  async act(action: Action): Promise<ActionResult> {
    try {
      switch (action.type) {
        case "device.reload": {
          const t = this.#target(action.key);
          if (!t) return GONE;
          t.session.reload(t.id);
          return { ok: true };
        }
        case "device.connect":
        case "device.disconnect":
        case "device.unpair":
        case "device.refresh": {
          const t = this.#target(action.key);
          if (!t) return GONE;
          const { session, id, device } = t;
          if (action.type === "device.refresh" && device.state !== "connected") return { ok: false, message: codeText("not_connected") };
          if (action.type === "device.connect") {
            if (device.inactive === "transport_disabled" && device.transport) return { ok: false, message: transportDisabledText(device.transport) };
            try {
              await session.perform(id, "connect", (c) => c.connectDevice(id));
            } catch (error) {
              return { ok: false, message: transportFailure(error, session, [device.transport]) };
            }
          } else if (action.type === "device.disconnect") await session.perform(id, "disconnect", (c) => c.disconnectDevice(id));
          else if (action.type === "device.unpair") await session.perform(id, "unpair", (c) => c.unpairDevice(id));
          else await session.perform(id, "refresh", (c) => c.refreshDevice(id));
          return { ok: true };
        }
        case "device.enabled":
        case "device.trusted":
        case "device.blocked":
        case "device.hidpp": {
          const t = this.#target(action.key);
          if (!t) return GONE;
          const { session, id, device } = t;
          if (action.type === "device.enabled" && action.value && !device.enabled && enabledFull(session.status, [...session.devices.values()], device))
            return { ok: false, message: errorText({ code: "no_capacity", reason: "enabled", outcomeUnknown: false }) };
          if (action.type === "device.hidpp" && settingsBusy(this.#device(session.adapterId, session, device)))
            return { ok: false, message: codeText("busy") };
          const field = action.type.slice("device.".length) as "enabled" | "trusted" | "blocked" | "hidpp";
          await session.perform(id, field, (c) =>
            c.setDevice(
              field === "hidpp"
                ? { device: id, integrations: [{ kind: device.hidpp?.kind ?? IntegrationKind.HIDPP, enabled: action.value }] }
                : { device: id, [field]: action.value },
            ),
          );
          return { ok: true };
        }
        case "settings.save":
          return await this.#saveSettings(action.key, action.changes);
        case "adapter.name":
        case "adapter.platform":
        case "adapter.transport": {
          const session = this.#session(action.adapterId);
          if (!session) return GONE;
          if (!session.status.ready) return { ok: false, message: codeText("not_ready") };
          if (action.type === "adapter.name") {
            const name = action.name === null ? "" : adapterName(action.name);
            if (name === null) return { ok: false, message: "Invalid adapter name" };
            await session.connection.setAdapter({ name });
          } else if (action.type === "adapter.platform") await session.connection.setAdapter({ platform: convert.wire(Platform, action.platform) });
          else {
            if (!(action.transport in TRANSPORTS) || !session.status.transports.some((t) => t.transport === action.transport && t.settable))
              return { ok: false, message: codeText("unsupported") };
            await session.connection.setAdapter({ transports: [{ transport: convert.wire(Transport, action.transport), enabled: action.enabled }] });
          }
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
          return await this.#startScan(action.adapterId);
        case "scan.stop":
          await this.#stopScan();
          return { ok: true };
        case "pair.start":
          return await this.#pair(action.adapterId, action.candidateId);
        case "pair.reply":
          return await this.#reply(action.accept, action.value);
        case "pair.cancel":
          if (this.#pairing?.phase === "pairing") await this.#pairing.session.connection.cancelPairing();
          return { ok: true };
        case "pair.dismiss":
          // A pairing still finishing is forgotten when it ends; a saved
          // device's connection continues and shows on its page.
          if (this.#pairing) {
            if (this.#pairing.phase === "pairing") this.#pairing.dismissed = true;
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

  #answered(session: AdapterSession, request: Request, response: Response) {
    if (request.command.case !== "startScan") return;
    // Each session answers its requests in order, so this is its oldest unanswered scan.
    const i = this.#starts.findIndex((s) => s.scan.session === session);
    if (i === -1) return;
    const { scan, stopped } = this.#starts.splice(i, 1)[0]!;
    if (stopped) return;
    if (response.result.case === "error") {
      scan.running = false;
      const error = new CordialError("startScan", response.result.value.code, response.result.value.reason, response.result.value.outcomeUnknown);
      scan.error = transportFailure(error, session, request.command.value.transports.map(convert.transport));
    }
    this.#scan = scan;
    this.changed();
  }

  #event(session: AdapterSession, event: Event) {
    const kind = event.kind;
    const scan = this.#scan?.session === session ? this.#scan : null;
    const pairing = this.#pairing?.session === session ? this.#pairing : null;
    if (kind.case === "scanFound" && scan) {
      const c = convert.candidate(kind.value);
      const i = scan.candidates.findIndex((x) => x.id === c.id);
      if (i === -1) scan.candidates.push(c);
      else scan.candidates[i] = c;
    } else if (kind.case === "scanDone" && scan) {
      scan.running = false;
    } else if (kind.case === "pairing" && pairing && kind.value.candidate === pairing.candidateId && pairing.phase === "pairing") {
      const step = convert.pairingStep(kind.value);
      if (step.kind === "progress") pairing.prompt = null;
      else if (step.kind === "prompt") pairing.prompt = step.prompt;
      else if (step.kind === "done") {
        pairing.prompt = null;
        pairing.deviceKey = `${pairing.adapterId}/${step.device}`;
        pairing.phase = "connecting";
        // A dismissed pairing that saved its device is done with.
        if (pairing.dismissed) this.#pairing = null;
      } else {
        pairing.prompt = null;
        pairing.phase = step.code === "cancelled" ? "cancelled" : "failed";
        pairing.message =
          step.code === "cancelled" ? "Pairing was cancelled."
            : step.code === "unsupported" && pairing.transport && disabled(session, pairing.transport) ? transportDisabledText(pairing.transport)
              : codeText(step.code);
        if (pairing.dismissed) this.#pairing = null;
      }
    } else return;
    this.changed();
  }

  async #startScan(adapterId: string): Promise<ActionResult> {
    const session = this.#session(adapterId);
    if (!session) return { ok: false, message: "This adapter is no longer available." };
    const supported = session.status.transports;
    if (!supported.length) return { ok: false, message: "This adapter can't search for devices." };
    const transports = supported.filter((t) => t.enabled).map((t) => convert.wire(Transport, t.transport));
    // With every transport disabled, the one named is the last listed: BLE, when there are two.
    if (!transports.length) return { ok: false, message: transportDisabledText(supported.at(-1)!.transport) };
    if (this.#scan?.running && this.#scan.session !== session) await this.#stopScan();
    // Events before the adapter answers the scan still belong to the earlier one; the new scan
    // replaces it, and its candidates, at its response.
    const scan: Scan = { adapterId, running: true, candidates: [], error: null, session };
    this.#starts.push({ scan, stopped: false });
    try {
      await session.connection.startScan(transports, SCAN_SECONDS);
    } catch (error) {
      return { ok: false, message: transportFailure(error, session, transports.map(convert.transport)) };
    }
    return { ok: true };
  }

  async #stopScan() {
    const scan = this.#scan;
    const pending = this.#starts.filter((s) => !s.stopped);
    for (const s of pending) s.stopped = true;
    if (!scan && !pending.length) return;
    this.#scan = null;
    this.changed();
    // A stop sent after an unanswered start ends the scan that start begins.
    const sessions = new Set([...(scan?.running ? [scan.session] : []), ...pending.map((s) => s.scan.session)]);
    for (const session of sessions) await session.connection.stopScan().catch(() => {});
  }

  async #pair(adapterId: string, candidateId: string): Promise<ActionResult> {
    const session = this.#session(adapterId);
    if (!session) return { ok: false, message: "This adapter is no longer available." };
    if (this.#pairing?.phase === "pairing" && !this.#pairing.dismissed) return { ok: false, message: "Another device is being added." };
    if (storageFull(session.status)) return { ok: false, message: errorText({ code: "no_capacity", reason: "storage", outcomeUnknown: false }) };
    const candidate = this.#scan?.candidates.find((c) => c.id === candidateId);
    const transport = candidate?.transport ?? null;
    if (transport && disabled(session, transport)) return { ok: false, message: transportDisabledText(transport) };
    // Candidates stay usable after their scan stops.
    if (this.#scan?.running && this.#scan.session === session) {
      this.#scan.running = false;
      await session.connection.stopScan().catch(() => {});
    }
    const pairing: Pairing = {
      adapterId,
      candidateId,
      name: clean(candidate?.name ?? "") || "the device",
      phase: "pairing",
      prompt: null,
      deviceKey: null,
      message: null,
      session,
      dismissed: false,
      transport,
    };
    this.#pairing = pairing;
    this.changed();
    try {
      await session.connection.startPairing(candidateId);
    } catch (error) {
      if (this.#pairing === pairing) {
        pairing.phase = "failed";
        pairing.message = transportFailure(error, session, [transport]);
        if (pairing.dismissed) this.#pairing = null;
        this.changed();
      }
    }
    return { ok: true };
  }

  async #reply(accept: boolean, value?: string): Promise<ActionResult> {
    const pairing = this.#pairing;
    const prompt = pairing?.prompt;
    if (!pairing || !prompt || prompt.kind === "show") return { ok: false, message: "No pairing prompt is waiting." };
    try {
      if (accept) await pairing.session.connection.acceptPrompt(prompt.kind === "enter" ? (value ?? "") : "");
      else await pairing.session.connection.rejectPrompt();
      if (pairing.prompt === prompt) pairing.prompt = null;
      this.changed();
      return { ok: true };
    } catch (error) {
      if (error instanceof CordialError && convert.errorCode(error.code) === "no_prompt") {
        if (pairing.prompt === prompt) pairing.prompt = null;
        this.changed();
      }
      return failed(error);
    }
  }

  async stop() {
    await this.manager.stop();
  }
}
