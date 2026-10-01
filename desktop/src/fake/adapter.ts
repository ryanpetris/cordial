// A simulated Cordial adapter speaking protocol version 1 over an in-memory
// Transport. Tests and the demo mode use it; all data is synthetic.
import { adapterName } from "../shared/adapter-name.ts";
import type {
  Candidate,
  Device,
  ErrorCode,
  HostPlatform,
  InfoField,
  InfoKey,
  ProtocolState,
  Setting,
  SettingValue,
  Status,
} from "../protocol/types.ts";
import type { Transport } from "../core/transport.ts";

type Json = Record<string, unknown>;

export interface FakeDevice {
  device: Device;
  info: InfoField[];
  settings: Setting[];
}

export interface FakeOptions {
  adapterId?: string;
  board?: string;
  capabilities?: Status["capacity"]["pairing"][number]["transport"][];
  devices?: FakeDevice[];
  candidates?: Candidate[];
  /** How pairing authenticates; null pairs without a prompt. */
  pairingMethod?: "confirm_passkey" | "enter_passkey" | "passkey" | null;
  /** Responds with this error code to wait_ready. */
  readyError?: string;
  /** Milliseconds before each reply. */
  latency?: number;
  /** Milliseconds between saving a setting and finishing its hardware job. */
  settingJobMs?: number;
}

export function device(id: string, patch: Partial<Device> = {}): Device {
  return {
    device_id: id,
    pairing_state: "paired",
    name: "Keyboard",
    transport: "ble",
    roles: [],
    state: "disconnected",
    security: null,
    enabled: true,
    effective_enabled: true,
    enabled_reason: null,
    transport_supported: true,
    validation_error: null,
    trusted: true,
    blocked: false,
    reconnect: "auto",
    last_error: null,
    hidpp_enabled: true,
    hidpp_protocol: { state: "unknown" },
    normalization_state: "pending",
    normalization_error: null,
    settings_state: "pending",
    settings_error: null,
    settings_revision: 0,
    ...patch,
  };
}

const INFO_KEYS: [InfoKey, number][] = [
  ["name", 0],
  ["kind", 0],
  ["manufacturer", 0],
  ["model", 0],
  ["serial", 0],
  ["firmware", 0],
  ["firmware", 1],
  ["hardware", 0],
  ["software", 0],
  ["vendor_id_namespace", 0],
  ["vendor_id", 0],
  ["product_id", 0],
  ["product_version", 0],
  ["battery_percent", 0],
  ["battery_charging", 0],
];

/** A complete information snapshot from the given known values. */
export function info(values: Partial<Record<InfoKey | "bootloader", SettingValue>>, fresh = true): InfoField[] {
  return INFO_KEYS.map(([key, instance]) => {
    const value = (instance === 1 ? values.bootloader : values[key]) ?? null;
    return { key, instance, value, available: value !== null, fresh: value !== null && fresh };
  });
}

export function setting(key: Setting["key"], patch: Partial<Setting>): Setting {
  return {
    key,
    type: "bool",
    writable: true,
    feature: 6530,
    feature_version: 1,
    scope: "device",
    choices: [],
    min: null,
    max: null,
    step: null,
    managed: false,
    desired: null,
    observed: null,
    fresh: true,
    observed_at_ms: 1000,
    observation_source: "read",
    state: "unmanaged",
    error: null,
    ...patch,
  };
}

/** A few synthetic devices showing the main states. */
export function demoDevices(): FakeDevice[] {
  const connected = { encrypted: true, authenticated: false, secure_connections: true, key_size: 16, bonded: true };
  return [
    {
      device: device("d_1", {
        name: "Example Keys",
        state: "connected",
        roles: ["keyboard", "consumer_control"],
        security: connected,
        normalization_state: "active",
        hidpp_protocol: { state: "detected", major: 4, minor: 5 },
        settings_state: "ready",
        settings_revision: 3,
      }),
      info: info({
        name: "Example Keys Wireless",
        kind: "keyboard",
        manufacturer: "Example Co",
        model: "Example Keys",
        serial: "0000EXAMPLE1",
        firmware: "EK 1.02.0003",
        bootloader: "BL 1.00.0001",
        vendor_id_namespace: "usb",
        vendor_id: 0x1234,
        product_id: 0x5678,
        product_version: 1,
        battery_percent: 72,
        battery_charging: false,
      }),
      settings: [
        setting("fn.row_default", {
          type: "enum",
          feature: 16547,
          feature_version: 0,
          scope: "current_host",
          choices: ["function_keys", "special_actions"],
          observed: "special_actions",
        }),
        setting("backlight.enabled", { observed: true, managed: true, desired: true, state: "applied" }),
        setting("backlight.mode", {
          type: "enum",
          choices: ["automatic", "permanent_manual"],
          observed: "automatic",
        }),
        setting("backlight.level", { type: "integer", min: 0, max: 7, step: 1, observed: 3 }),
        setting("backlight.delay.hands_out", {
          type: "integer",
          min: 5,
          max: 7200,
          step: 5,
          observed: 60,
          managed: true,
          desired: 30,
          state: "changed_on_device",
        }),
        setting("backlight.current_level", { type: "integer", writable: false, min: 0, max: 7, step: 1, observed: 3 }),
      ],
    },
    {
      device: device("d_2", {
        name: "Example Mouse",
        state: "connected",
        roles: ["mouse"],
        security: connected,
        normalization_state: "active",
        hidpp_protocol: { state: "detected", major: 4, minor: 5 },
        settings_state: "ready",
        settings_revision: 3,
      }),
      info: info({ name: "Example Mouse", kind: "mouse", manufacturer: "Example Co", battery_percent: 12, battery_charging: false }),
      settings: [
        setting("pointer.dpi.0", { type: "integer", feature: 8705, min: 200, max: 8000, step: 50, observed: 1000 }),
        setting("wheel.mode", { type: "enum", feature: 8464, choices: ["freespin", "ratchet"], observed: "ratchet" }),
        setting("wheel.invert", { feature: 8464, observed: false }),
        setting("wheel.info", { type: "text", writable: false, feature: 8464, observed: "080c1832" }),
      ],
    },
    {
      device: device("d_3", { name: "Travel Keyboard", transport: "classic", hidpp_enabled: false, normalization_state: "off" }),
      info: info({ name: "Travel Keyboard", kind: "keyboard", firmware: "2.1" }, false),
      settings: [],
    },
    {
      device: device("d_4", {
        name: "Old Mouse",
        pairing_state: "needs_pairing",
        validation_error: "bond_missing",
        effective_enabled: false,
        enabled_reason: "invalid",
      }),
      info: info({}),
      settings: [],
    },
  ];
}

export function demoCandidates(): Candidate[] {
  return [
    { candidate_id: "c_1", kind: "keyboard", name: "Example Keys Mini", transport: "ble", rssi: -48 },
    { candidate_id: "c_2", kind: "mouse", name: "Example Pebble", transport: "ble", rssi: -63 },
    { candidate_id: "c_3", kind: "keyboard", name: "Example Classic Keyboard", transport: "classic", rssi: -71 },
    { candidate_id: "c_4", kind: "unknown", name: null, transport: "ble", rssi: -80 },
  ];
}

export class FakeAdapter implements Transport {
  readonly id: string;
  readonly board: string;
  devices: FakeDevice[];
  candidates: Candidate[];
  pairingMethod: FakeOptions["pairingMethod"];
  capabilities: string[];
  platform: HostPlatform = "linux";
  name: string;
  readonly defaultName: string;
  #customName = false;
  revision = 0;
  monitor = false;
  /** Requests the adapter has received in this session, for assertions. */
  received: Json[] = [];
  /** Error codes to answer the next requests of a command with, in order. */
  failures: Record<string, string[]> = {};
  /** Hardware failures after a setting has already been saved. */
  settingFailures: Partial<Record<Setting["key"], ErrorCode>> = {};
  /** Input from an earlier session delivered right after the next open. */
  staleInput: string | null = null;
  readonly #readyError: string | undefined;
  readonly #latency: number;
  readonly #settingJobMs: number;
  #data: ((chunk: Uint8Array) => void)[] = [];
  #close: ((error: Error | null) => void)[] = [];
  #session = 0;
  #open = false;
  #lastId = 0;
  #scan: number | null = null;
  #pairing: { id: number; candidate: Candidate; prompt: string | null } | null = null;
  #nextDevice = 10;
  readonly #protocols = new Map<string, ProtocolState>();

  constructor(options: FakeOptions = {}) {
    this.id = options.adapterId ?? "0000FAKE0001";
    this.board = options.board ?? "pico_w";
    this.name = ({ pico_w: "Pico W", pico2_w: "Pico 2 W", xiao_esp32s3: "XIAO ESP32-S3", waveshare_rp2350b_plus_w: "RP2350B-Plus-W" } as Record<string, string>)[this.board] ?? this.board;
    this.defaultName = this.name;
    this.devices = options.devices ?? demoDevices();
    for (const d of this.devices) this.#protocols.set(d.device.device_id, d.device.hidpp_protocol ?? { state: "unknown" });
    this.candidates = options.candidates ?? demoCandidates();
    this.pairingMethod = options.pairingMethod === undefined ? "confirm_passkey" : options.pairingMethod;
    this.capabilities = options.capabilities ?? ["classic", "ble"];
    this.#readyError = options.readyError;
    this.#latency = options.latency ?? 0;
    this.#settingJobMs = options.settingJobMs ?? 0;
  }

  // ---- Transport ----------------------------------------------------------

  onData(listener: (chunk: Uint8Array) => void) {
    this.#data.push(listener);
  }
  onClose(listener: (error: Error | null) => void) {
    this.#close.push(listener);
  }
  async write(text: string) {
    if (!this.#open) throw new Error("port closed");
    for (const line of text.split("\n").filter(Boolean)) {
      const message = JSON.parse(line) as Json;
      this.received.push(message);
      setTimeout(() => this.#handle(message), this.#latency);
    }
  }
  async close() {
    this.#open = false;
    this.#close = [];
    this.#data = [];
    this.#scan = null;
    this.#pairing = null;
    this.monitor = false;
  }

  /** Opens the port: DTR rises and a new control session starts. */
  open() {
    this.#open = true;
    this.#session++;
    this.monitor = false;
    this.#lastId = 0;
    this.#scan = null;
    this.#pairing = null;
    this.received = [];
    const stale = this.staleInput;
    this.staleInput = null;
    setTimeout(() => this.#send(`${stale ?? ""}\n`), 1);
    return this;
  }

  /** Simulates unplugging: the port fails. */
  unplug() {
    this.#open = false;
    for (const listener of this.#close.splice(0)) listener(new Error("device disconnected"));
  }

  // ---- Simulation controls ----------------------------------------------

  #send(text: string) {
    if (!this.#open) return;
    const bytes = new TextEncoder().encode(text);
    for (const listener of this.#data) listener(bytes);
  }
  #line(message: Json) {
    this.#send(`${JSON.stringify({ v: 1, ...message })}\n`);
  }
  #event(event: string, data: unknown, requestId?: number) {
    this.#line({ type: "event", event, data, ...(requestId ? { request_id: requestId } : {}) });
  }
  #bump(): number {
    return ++this.revision;
  }

  find(id: string): FakeDevice {
    const d = this.devices.find((x) => x.device.device_id === id);
    if (!d) throw new Error(`no fake device ${id}`);
    return d;
  }

  /** Changes a device record and reports it like the firmware would. */
  changeDevice(id: string, patch: Partial<Device>, event = "device.changed") {
    const d = this.find(id);
    if (patch.hidpp_protocol) this.#protocols.set(id, patch.hidpp_protocol);
    if (patch.state && patch.state !== "connected") patch = { ...patch, hidpp_protocol: { state: "unknown" } };
    else if (patch.state === "connected" && d.device.state !== "connected" && !patch.hidpp_protocol) {
      patch = { ...patch, hidpp_protocol: this.#protocols.get(id) ?? { state: "unknown" } };
    }
    d.device = { ...d.device, ...patch };
    const revision = this.#bump();
    if (this.monitor) this.#event(event, { revision, device: d.device, ...(event === "device.disconnected" ? { reason: "remote" } : {}) });
  }

  /** Updates information fields and sends only the changes. */
  changeInfo(id: string, values: Partial<Record<InfoKey, SettingValue>>) {
    const d = this.find(id);
    const changed: InfoField[] = [];
    for (const [key, value] of Object.entries(values) as [InfoKey, SettingValue][]) {
      const f = d.info.find((x) => x.key === key && x.instance === 0)!;
      Object.assign(f, { value, available: value !== null, fresh: value !== null });
      changed.push({ ...f });
    }
    const revision = this.#bump();
    if (this.monitor) this.#event("device.info.changed", { revision, device_id: id, fields: changed });
  }

  /** Discards notifications, as when output backs up. */
  loseEvents(count = 2) {
    this.revision += count;
    if (this.monitor) this.#event("events.lost", { revision: this.revision, dropped: count });
  }

  status(): Status {
    const counts = {
      saved: this.devices.length,
      paired: this.devices.filter((d) => d.device.pairing_state === "paired").length,
      preferred_enabled: this.devices.filter((d) => d.device.enabled).length,
      enabled: this.devices.filter((d) => d.device.effective_enabled).length,
      connected: this.devices.filter((d) => d.device.state === "connected").length,
    };
    const transports = this.capabilities.filter((c) => c === "ble" || c === "classic") as ("ble" | "classic")[];
    return {
      protocol: 1,
      firmware_version: "0.0.0",
      hardware_config: this.board,
      hardware_digest: "0".repeat(64),
      radio_backend: "pico-sdk-cyw43",
      adapter_id: this.id,
      build_profile: "development",
      boot_id: "fakeboot",
      session_id: `fakeboot-${this.#session}`,
      limits: {
        max_line_bytes: 4096,
        max_pending_requests: 4,
        saved_devices: 64,
        active_connections: 4,
        scan_candidates: 32,
        hidpp_settings: 20,
        hidpp_saved_settings: 20,
        hidpp_sensors: 2,
        hidpp_firmware_entities: 2,
        hidpp_setting_choices: 65536,
        hidpp_features: 256,
      },
      counts,
      capacity: {
        enabled: transports.map((t) => ({ transports: [t], limit: 7, enabled: this.devices.filter((d) => d.device.transport === t && d.device.effective_enabled).length })),
        pairing: transports.map((t) => ({ transport: t, available: true, reason: null, estimated_additional: 40 })),
      },
      revision: this.revision,
      host_platform: this.platform,
      name: this.name,
      monitor: this.monitor,
      radio_ready: !this.#readyError,
      storage_ready: true,
      heartbeat: { interval_ms: 5000, timeout_ms: 15000, remaining_ms: 15000 },
      pending: [],
    };
  }

  // ---- Commands ----------------------------------------------------------

  #ok(id: number, result: unknown, done = true) {
    this.#line({ type: "response", id, ok: true, done, result });
  }
  #error(id: number, code: string, details?: unknown) {
    this.#line({ type: "response", id, ok: false, done: true, error: { code, ...(details ? { details } : {}) } });
  }

  #handle(m: Json) {
    const id = m.id as number;
    if (id <= this.#lastId) return;
    this.#lastId = id;
    if (m.cmd === "adapter.protocol")
      return this.#line({ v: 0, type: "response", id, ok: true, done: true, result: { protocol: 1 } });
    const args = m.args as Json;
    const target = typeof args.device_id === "string" ? this.devices.find((d) => d.device.device_id === args.device_id) : undefined;
    const failure = this.failures[m.cmd as string]?.shift();
    if (failure) return this.#error(id, failure);
    const needDevice = () => {
      if (!target) this.#error(id, "not_found");
      return target;
    };
    switch (m.cmd) {
      case "adapter.capabilities":
        return this.#ok(id, this.capabilities);
      case "adapter.status":
        return this.#ok(id, this.status());
      case "session.heartbeat":
        return this.#ok(id, { timeout_ms: 15000, monitor: this.monitor });
      case "adapter.wait_ready":
        if (this.#readyError) return this.#error(id, this.#readyError);
        return this.#ok(id, { state: "ready", status: this.status() });
      case "session.monitor.set":
        this.monitor = args.enabled as boolean;
        return this.#ok(id, { enabled: this.monitor, revision: this.revision });
      case "device.list":
        for (const d of this.devices) this.#ok(id, { revision: this.revision, device: d.device }, false);
        return this.#ok(id, { count: this.devices.length, revision: this.revision });
      case "device.info":
      case "device.info.refresh": {
        const d = needDevice();
        if (!d) return;
        if (m.cmd === "device.info.refresh" && d.device.state !== "connected") return this.#error(id, "not_connected");
        return this.#ok(id, { revision: this.revision, device_id: d.device.device_id, fields: d.info });
      }
      case "hidpp.setting.list": {
        const d = needDevice();
        if (!d) return;
        for (const s of d.settings) this.#ok(id, { revision: this.revision, device_id: d.device.device_id, setting: s }, false);
        return this.#ok(id, {
          revision: this.revision,
          device_id: d.device.device_id,
          count: d.settings.length,
          settings_state: d.device.settings_state,
          settings_error: null,
        });
      }
      case "hidpp.setting.set":
      case "hidpp.setting.forget": {
        const d = needDevice();
        if (!d) return;
        const i = d.settings.findIndex((s) => s.key === args.key);
        if (i === -1) return this.#error(id, "not_found");
        const s = d.settings[i]!;
        if (!s.writable) return this.#error(id, "read_only");
        if (d.device.settings_state === "applying") return this.#error(id, "busy");
        if (m.cmd === "hidpp.setting.set" && d.device.state !== "connected") return this.#error(id, "not_connected");
        const apply = m.cmd === "hidpp.setting.set" && d.device.hidpp_enabled;
        const next =
          m.cmd === "hidpp.setting.set"
            ? { ...s, managed: true, desired: args.value as SettingValue, error: null,
                ...(apply && !this.#settingJobMs ? { observed: args.value as SettingValue, state: "applied" as const } : { state: "pending" as const }) }
            : { ...s, managed: false, desired: null, state: "unmanaged" as const };
        d.settings[i] = next;
        if (apply && this.#settingJobMs) this.changeDevice(d.device.device_id, { settings_state: "applying" });
        const revision = this.#bump();
        this.#ok(id, { revision, device_id: d.device.device_id, setting: next });
        if (apply && this.#settingJobMs) {
          const session = this.#session;
          setTimeout(() => {
            if (!this.#open || this.#session !== session || d.device.state !== "connected") return;
            const error = this.settingFailures[next.key];
            const applied = { ...next, error: error ?? null, state: error ? "error" as const : "applied" as const,
              observed: error ? s.observed : next.desired, fresh: !error };
            d.settings[i] = applied;
            const revision = this.#bump();
            if (this.monitor) this.#event("hidpp.setting.changed", { revision, device_id: d.device.device_id, setting: applied });
            this.changeDevice(d.device.device_id, { settings_state: "ready", settings_revision: revision });
          }, this.#settingJobMs);
        }
        return;
      }
      case "hidpp.setting.refresh":
      case "hidpp.setting.apply": {
        const d = needDevice();
        if (!d) return;
        if (d.device.state !== "connected") return this.#error(id, "not_connected");
        const outcome = m.cmd === "hidpp.setting.refresh" ? "read" : "unchanged";
        for (const s of d.settings) this.#ok(id, { revision: this.revision, device_id: d.device.device_id, setting: s, outcome }, false);
        const summary = { revision: this.revision, device_id: d.device.device_id, count: d.settings.length, read: 0, applied: 0, unchanged: 0, unsupported: 0, failed: 0, uncertain: 0 };
        summary[outcome] = d.settings.length;
        return this.#ok(id, summary);
      }
      case "device.connect": {
        const d = needDevice();
        if (!d) return;
        if (d.device.blocked) return this.#error(id, "blocked");
        if (d.device.pairing_state === "needs_pairing") return this.#error(id, "pairing_required");
        if (!d.device.enabled) return this.#error(id, "disabled");
        this.changeDevice(d.device.device_id, { state: "connecting", reconnect: "auto" });
        setTimeout(() => {
          this.changeDevice(d.device.device_id, { state: "connected", roles: d.device.roles.length ? d.device.roles : ["keyboard"] }, "device.connected");
          this.#ok(id, { device: d.device });
        }, 20);
        return;
      }
      case "device.disconnect": {
        const d = needDevice();
        if (!d) return;
        const wasConnected = d.device.state === "connected";
        this.changeDevice(d.device.device_id, { state: "disconnected", reconnect: "paused", security: null }, wasConnected ? "device.disconnected" : "device.changed");
        this.changeInfo(d.device.device_id, { battery_percent: null, battery_charging: null });
        return this.#ok(id, { device: d.device });
      }
      case "device.unpair": {
        const d = needDevice();
        if (!d) return;
        this.devices = this.devices.filter((x) => x !== d);
        const revision = this.#bump();
        if (this.monitor) this.#event("device.unpaired", { revision, device_id: d.device.device_id });
        return this.#ok(id, { device_id: d.device.device_id, removed: true });
      }
      case "device.enabled.set":
      case "device.trusted.set":
      case "device.blocked.set":
      case "device.hidpp.set": {
        const d = needDevice();
        if (!d) return;
        const patch: Partial<Device> =
          m.cmd === "device.enabled.set"
            ? { enabled: args.enabled as boolean, effective_enabled: args.enabled as boolean, enabled_reason: args.enabled ? null : "disabled" }
            : m.cmd === "device.trusted.set"
              ? { trusted: args.trusted as boolean }
              : m.cmd === "device.blocked.set"
                ? { blocked: args.blocked as boolean }
                : { hidpp_enabled: args.enabled as boolean, normalization_state: args.enabled ? "active" : "off" };
        this.changeDevice(d.device.device_id, patch);
        return this.#ok(id, { device: d.device });
      }
      case "adapter.name.set":
      case "adapter.platform.set": {
        const reset = m.cmd === "adapter.name.set" && args.name === null;
        const name = reset ? this.defaultName : m.cmd === "adapter.name.set" ? adapterName(args.name as string) : this.name;
        if (name === null) return this.#error(id, "invalid_args");
        const platform = m.cmd === "adapter.platform.set" ? args.platform as HostPlatform : this.platform;
        const changed = name !== this.name || platform !== this.platform || reset && this.#customName;
        if (m.cmd === "adapter.name.set" && changed) this.#customName = !reset;
        this.name = name;
        this.platform = platform;
        const revision = changed ? this.#bump() : this.revision;
        const result = { revision, host_platform: this.platform, name: this.name };
        if (changed && this.monitor) this.#event("adapter.changed", result);
        return this.#ok(id, result);
      }
      case "discovery.scan": {
        if (this.#scan) return this.#error(id, "busy");
        this.#scan = id;
        this.candidates.forEach((c, i) =>
          setTimeout(() => {
            if (this.#scan === id) this.#event("discovery.result", c, id);
          }, 30 * (i + 1)),
        );
        if (args.duration_ms !== 0)
          setTimeout(() => {
            if (this.#scan !== id) return;
            this.#scan = null;
            this.#ok(id, { count: this.candidates.length, truncated: false });
          }, 1000);
        return;
      }
      case "request.cancel": {
        const target = args.request_id as number;
        if (this.#scan === target) {
          this.#ok(id, { request_id: target, requested: true });
          this.#scan = null;
          this.#error(target, "cancelled");
        } else if (this.#pairing?.id === target) {
          this.#ok(id, { request_id: target, requested: true });
          this.#pairing = null;
          this.#error(target, "cancelled");
        } else this.#error(id, "not_pending");
        return;
      }
      case "pairing.start": {
        const candidate = this.candidates.find((c) => c.candidate_id === args.candidate_id);
        if (!candidate) return this.#error(id, "candidate_expired");
        this.#pairing = { id, candidate, prompt: null };
        const method = this.pairingMethod;
        if (!method) return setTimeout(() => this.#paired(), 20);
        const promptId = "p_1";
        this.#pairing.prompt = promptId;
        const data = {
          candidate_id: candidate.candidate_id,
          prompt_id: promptId,
          method,
          expires_in_ms: 30000,
          ...(method === "enter_passkey" ? {} : { value: "042731" }),
        };
        setTimeout(() => this.#event(method === "passkey" ? "pairing.display" : "pairing.prompt", data, id), 20);
        if (method === "passkey") setTimeout(() => this.#paired(), 200);
        return;
      }
      case "pairing.reply": {
        const p = this.#pairing;
        if (!p || p.id !== args.request_id || p.prompt !== args.prompt_id) return this.#error(id, "stale_prompt");
        p.prompt = null;
        this.#ok(id, { accepted: true });
        if (args.action === "reject") {
          this.#pairing = null;
          this.#error(p.id, "authentication_rejected");
        } else setTimeout(() => this.#paired(), 20);
        return;
      }
      default:
        return this.#error(id, "unknown_command");
    }
  }

  #paired() {
    const p = this.#pairing;
    if (!p) return;
    this.#pairing = null;
    const created = device(`d_${this.#nextDevice++}`, {
      name: p.candidate.name,
      transport: p.candidate.transport,
      state: "connecting",
    });
    this.devices.push({ device: created, info: info({ name: p.candidate.name, battery_percent: 90 }), settings: [] });
    const revision = this.#bump();
    if (this.monitor) this.#event("device.paired", { revision, device: created });
    this.#ok(p.id, { device: created });
  }
}
