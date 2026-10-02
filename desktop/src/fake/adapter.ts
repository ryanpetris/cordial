// A simulated adapter speaking the serial API over an in-memory ByteStream,
// following the firmware's behavior: every command answers at once, and
// changes follow as events after the response. Tests and the demo mode use
// it; all data is synthetic.
import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import type { ByteStream } from "@cordial/client";
import {
  CapacityReason,
  CodeKind,
  DELIMITER,
  DeviceSchema,
  DeviceState,
  ErrorCode,
  FrameDecoder,
  InactiveReason,
  IntegrationKind,
  IntegrationState,
  Kind,
  MessageSchema,
  Platform,
  ReportType,
  RequestSchema,
  Role,
  SettingSchema,
  SettingState,
  StatusSchema,
  Transport,
  WarningCode,
  encodeFrame,
  type EventSchema,
  type PairingSchema,
  type Request,
  type ResponseSchema,
  type Value,
  type ValueSchema,
} from "@cordial/protocol";
import { adapterName } from "../shared/adapter-name.ts";
import type { Candidate, DeviceWarning, Scalar, ValueType } from "../shared/state.ts";

type MessageInit = MessageInitShape<typeof MessageSchema>;
type ValueInit = MessageInitShape<typeof ValueSchema>;

export interface FakeSetting {
  key: string;
  type: ValueType;
  value: Scalar | null;
  saved: Scalar | null;
  state: "pending" | "applied" | "changed_on_device" | "unsupported";
  /** The last apply's failure, replacing the state. */
  error: ErrorCode | null;
  choices: Scalar[];
  min: number | null;
  max: number | null;
  step: number | null;
}

export interface FakeDevice {
  id: string;
  transport: "classic" | "ble";
  name: string;
  kind: "unknown" | "keyboard" | "mouse" | "keyboard_mouse" | "other";
  state: "disconnected" | "connecting" | "connected" | "disconnecting";
  enabled: boolean;
  trusted: boolean;
  blocked: boolean;
  paused: boolean;
  error: ErrorCode | null;
  hidppEnabled: boolean;
  /** The HID++ version the device reports when connected; null for a device without HID++. */
  hidpp: [number, number] | null;
  /** Replaces the HID++ state the fake derives while connected, such as `STARTING`. */
  hidppState: IntegrationState | null;
  /** HID++ failed to start on the connected device. */
  hidppError: ErrorCode | null;
  roles: ("keyboard" | "mouse" | "consumer_control" | "system_control")[];
  /** What the device reports while connected, by catalog key. */
  info: Record<string, Scalar>;
  settings: FakeSetting[];
  warnings: DeviceWarning[];
}

export interface FakeOptions {
  adapterId?: string;
  board?: string;
  transports?: ("classic" | "ble")[];
  devices?: FakeDevice[];
  candidates?: Candidate[];
  /** How pairing authenticates; null pairs without a prompt. */
  pairingMethod?: "confirm" | "enter" | "show" | null;
  /** Bluetooth and storage have started. */
  ready?: boolean;
  /** Devices of one transport that can be enabled at once. */
  maxEnabled?: number;
  /** Milliseconds before each reply. */
  latency?: number;
  /** Milliseconds between saving settings and their apply finishing. */
  settingJobMs?: number;
}

export function device(id: string, patch: Partial<FakeDevice> = {}): FakeDevice {
  return {
    id,
    transport: "ble",
    name: "Keyboard",
    kind: "keyboard",
    state: "disconnected",
    enabled: true,
    trusted: true,
    blocked: false,
    paused: false,
    error: null,
    hidppEnabled: false,
    hidpp: null,
    hidppState: null,
    hidppError: null,
    roles: [],
    info: {},
    settings: [],
    warnings: [],
    ...patch,
  };
}

export function setting(key: string, patch: Partial<FakeSetting> = {}): FakeSetting {
  return { key, type: "bool", value: null, saved: null, state: "pending", error: null, choices: [], min: null, max: null, step: null, ...patch };
}

/** A few synthetic devices showing the main states. */
export function demoDevices(): FakeDevice[] {
  return [
    device("d_1", {
      name: "Example Keys Wireless",
      state: "connected",
      roles: ["keyboard", "consumer_control"],
      hidppEnabled: true,
      hidpp: [4, 5],
      info: {
        "device.manufacturer": "Example Co",
        "device.model": "Example Keys",
        "device.serial": "0000EXAMPLE1",
        "firmware.version": "EK 1.02.0003",
        "bootloader.version": "BL 1.00.0001",
        "vendor.registry": "usb",
        "vendor.id": 0x1234,
        "product.id": 0x5678,
        "product.version": 1,
        "battery.level": 72,
        "battery.charging": false,
        "backlight.current_level": 3,
      },
      settings: [
        setting("keyboard.fn_row", { type: "enum", choices: ["function_keys", "special_actions"], value: "special_actions" }),
        setting("backlight.enabled", { value: true, saved: true, state: "applied" }),
        setting("backlight.mode", { type: "enum", choices: ["automatic", "permanent_manual"], value: "automatic" }),
        setting("backlight.level", { type: "integer", min: 0, max: 7, step: 1, value: 3 }),
        setting("backlight.delay.hands_out", { type: "integer", min: 5, max: 7200, step: 5, value: 60, saved: 30, state: "changed_on_device" }),
      ],
    }),
    device("d_2", {
      name: "Example Mouse",
      kind: "mouse",
      state: "connected",
      roles: ["mouse"],
      hidppEnabled: true,
      hidpp: [4, 5],
      info: {
        "device.manufacturer": "Example Co",
        "battery.level": 12,
        "battery.charging": false,
        "wheel.resolution_multiplier": 8,
        "wheel.ratchets_per_rotation": 24,
        "wheel.diameter": 50,
      },
      warnings: [
        { code: "pointer_selector_unsupported", service: 1, reportId: 2, reportType: "input", bitOffset: 24, usagePage: 1, usage: 0x38 },
      ],
      settings: [
        setting("pointer.sensor.0.dpi", { type: "integer", min: 200, max: 8000, step: 50, value: 1000 }),
        setting("wheel.mode", { type: "enum", choices: ["freespin", "ratchet"], value: "ratchet" }),
        setting("wheel.invert", { value: false }),
      ],
    }),
    device("d_3", { name: "Travel Keyboard", transport: "classic", info: { "firmware.version": "2.1" } }),
    device("d_4", { name: "Old Mouse", kind: "mouse", enabled: false }),
  ];
}

export function demoCandidates(): Candidate[] {
  return [
    { id: "c_1", kind: "keyboard", name: "Example Keys Mini", transport: "ble", rssi: -48 },
    { id: "c_2", kind: "mouse", name: "Example Pebble", transport: "ble", rssi: -63 },
    { id: "c_3", kind: "keyboard", name: "Example Classic Keyboard", transport: "classic", rssi: -71 },
    { id: "c_4", kind: "unknown", name: "", transport: "ble", rssi: -80 },
  ];
}

const BOARDS: Record<string, string> = { pico_w: "Pico W", pico2_w: "Pico 2 W", xiao_esp32s3: "XIAO ESP32-S3", waveshare_rp2350b_plus_w: "RP2350B-Plus-W" };
const TRANSPORT = { classic: Transport.CLASSIC, ble: Transport.BLE } as const;
const upper = <E>(e: E, name: string) => (e as Record<string, number>)[name.toUpperCase()]!;

function wireValue(type: ValueType, v: Scalar): ValueInit {
  if (type === "bool") return { value: { case: "bool", value: v as boolean } };
  if (type === "integer") return { value: { case: "integer", value: BigInt(v as number) } };
  if (type === "color") return { value: { case: "color", value: v as number } };
  return { value: { case: "text", value: String(v) } };
}

/** A wire value as the setting's type takes it, or undefined when it doesn't match. */
function fromWire(type: ValueType, v: Value | undefined): Scalar | undefined {
  const value = v?.value;
  if (type === "bool" && value?.case === "bool") return value.value;
  if (type === "integer" && value?.case === "integer") return Number(value.value);
  if (type === "color" && value?.case === "color") return value.value;
  if ((type === "enum" || type === "text") && value?.case === "text") return value.value;
  return undefined;
}

class Refusal extends Error {
  constructor(readonly code: ErrorCode, readonly reason = CapacityReason.UNKNOWN) {
    super(ErrorCode[code]);
  }
}

export class FakeAdapter implements ByteStream {
  readonly id: string;
  readonly board: string;
  devices: FakeDevice[];
  candidates: Candidate[];
  pairingMethod: FakeOptions["pairingMethod"];
  transports: ("classic" | "ble")[];
  platform: "linux" | "windows" | "mac" = "linux";
  name: string;
  readonly defaultName: string;
  ready: boolean;
  storageFull = false;
  maxEnabled: number;
  /** Requests received in this session, for assertions. */
  received: Request[] = [];
  /** Error codes to answer the next requests of a command with, in order, by command case. */
  failures: Record<string, ErrorCode[]> = {};
  /** Apply failures for settings that are saved anyway. */
  settingFailures: Record<string, ErrorCode> = {};
  /** Bytes from an earlier session delivered right after the next open. */
  staleInput: Uint8Array | null = null;
  readonly #latency: number;
  readonly #settingJobMs: number;
  #data: ((chunk: Uint8Array) => void)[] = [];
  #close: ((error: Error | null) => void)[] = [];
  #decoder = new FrameDecoder(1024);
  #open = false;
  #starting = false;
  #session = 0;
  #scan: ReturnType<typeof setTimeout>[] | null = null;
  #pairing: { candidate: Candidate; prompt: boolean } | null = null;
  #nextDevice = 10;
  /** Replies and events waiting for the reply delay, in order. */
  #outbox: Promise<void> = Promise.resolve();
  #gate: Promise<void> | null = null;

  constructor(options: FakeOptions = {}) {
    this.id = options.adapterId ?? "0000FAKE0001";
    this.board = options.board ?? "pico_w";
    this.name = BOARDS[this.board] ?? this.board;
    this.defaultName = this.name;
    this.devices = options.devices ?? demoDevices();
    this.candidates = options.candidates ?? demoCandidates();
    this.pairingMethod = options.pairingMethod === undefined ? "confirm" : options.pairingMethod;
    this.transports = options.transports ?? ["classic", "ble"];
    this.ready = options.ready ?? true;
    this.maxEnabled = options.maxEnabled ?? 7;
    this.#latency = options.latency ?? 0;
    this.#settingJobMs = options.settingJobMs ?? 0;
  }

  // ---- ByteStream -----------------------------------------------------------

  onData(listener: (chunk: Uint8Array) => void) {
    this.#data.push(listener);
  }
  onClose(listener: (error: Error | null) => void) {
    this.#close.push(listener);
  }
  async write(bytes: Uint8Array) {
    if (!this.#open) throw new Error("port closed");
    if (this.#starting) {
      // What an earlier session left, then the new session's delimiter, reach
      // the client before any answer.
      this.#starting = false;
      const stale = this.staleInput ?? new Uint8Array();
      this.staleInput = null;
      this.#send(new Uint8Array([...stale, DELIMITER]));
    }
    for (const frame of this.#decoder.push(bytes)) {
      if (!frame.ok) {
        this.#reply({ result: { case: "error", value: { code: frame.error === "too_long" ? ErrorCode.TOO_LONG : ErrorCode.BAD_REQUEST } } });
        continue;
      }
      let request: Request;
      try {
        request = fromBinary(RequestSchema, frame.bytes);
      } catch {
        this.#reply({ result: { case: "error", value: { code: ErrorCode.BAD_REQUEST } } });
        continue;
      }
      this.received.push(request);
      this.#handle(request);
    }
  }
  async close() {
    this.#end();
    this.#close = [];
    this.#data = [];
  }

  /** Opens the port: DTR rises and a new session starts with a delimiter. */
  open() {
    this.#end();
    this.#open = true;
    this.#session++;
    this.received = [];
    this.#starting = true;
    return this;
  }

  /** Simulates unplugging: the port fails. */
  unplug() {
    this.#end();
    for (const listener of this.#close.splice(0)) listener(new Error("device disconnected"));
  }

  /** The session ends: a scan stops and an unsaved pairing is cancelled. */
  #end() {
    this.#open = false;
    for (const t of this.#scan ?? []) clearTimeout(t);
    this.#scan = null;
    this.#pairing = null;
    this.#decoder = new FrameDecoder(1024);
  }

  // ---- Output -------------------------------------------------------------

  #send(bytes: Uint8Array) {
    if (!this.#open) return;
    for (const listener of this.#data) listener(bytes);
  }

  /** Holds back everything the adapter writes until the returned function is called. */
  hold(): () => void {
    let release!: () => void;
    this.#gate = new Promise((r) => (release = r));
    return () => {
      this.#gate = null;
      release();
    };
  }

  /** Queues a message behind earlier ones, after the reply delay. */
  #message(message: MessageInit) {
    const session = this.#session;
    const frame = encodeFrame(toBinary(MessageSchema, create(MessageSchema, message)));
    const gate = this.#gate;
    this.#outbox = this.#outbox.then(async () => {
      if (gate) await gate;
      if (this.#latency) await new Promise((r) => setTimeout(r, this.#latency));
      else await Promise.resolve();
      if (this.#session === session) this.#send(frame);
    });
  }

  #reply(response: MessageInitShape<typeof ResponseSchema>) {
    this.#message({ kind: { case: "response", value: response } });
  }

  #event(event: MessageInitShape<typeof EventSchema>["kind"]) {
    this.#message({ kind: { case: "event", value: { kind: event } } });
  }

  // ---- Model ----------------------------------------------------------------

  find(id: string): FakeDevice {
    const d = this.devices.find((x) => x.id === id);
    if (!d) throw new Refusal(ErrorCode.NOT_FOUND);
    return d;
  }

  status() {
    const info: { key: string; value: ValueInit }[] = [
      { key: "firmware.version", value: wireValue("text", "0.0.0") },
      { key: "board.name", value: wireValue("text", this.board) },
      { key: "build.development", value: wireValue("bool", true) },
    ];
    if (this.storageFull) info.push({ key: "storage.full", value: wireValue("bool", true) });
    return create(StatusSchema, {
      id: this.id,
      name: this.name,
      platform: upper(Platform, this.platform),
      ready: this.ready,
      transports: this.transports.map((t) => ({ transport: TRANSPORT[t], maxEnabled: this.maxEnabled })),
      info,
    });
  }

  /** Why the adapter doesn't use `d`, as the firmware decides it: the first enabled devices fill each transport's places. */
  inactive(d: FakeDevice): InactiveReason | undefined {
    if (!this.transports.includes(d.transport)) return InactiveReason.UNSUPPORTED_TRANSPORT;
    if (d.blocked) return InactiveReason.BLOCKED;
    if (!d.enabled) return InactiveReason.DISABLED;
    const before = this.devices.filter((x) => x.transport === d.transport && x.enabled && !x.blocked);
    return before.indexOf(d) >= this.maxEnabled ? InactiveReason.CAPACITY : undefined;
  }

  #hidpp(d: FakeDevice) {
    const connected = d.state === "connected";
    const detected = connected && d.hidpp !== null;
    if (!d.hidppEnabled && !detected) return [];
    let status: { case: "state"; value: IntegrationState } | { case: "error"; value: ErrorCode };
    if (!d.hidppEnabled) status = { case: "state", value: IntegrationState.OFF };
    else if (!connected) status = { case: "state", value: IntegrationState.DISCONNECTED };
    else if (d.hidppError !== null) status = { case: "error", value: d.hidppError };
    else if (d.hidppState !== null) status = { case: "state", value: d.hidppState };
    else status = { case: "state", value: d.hidpp ? IntegrationState.ACTIVE : IntegrationState.UNSUPPORTED };
    return [{
      kind: IntegrationKind.HIDPP,
      enabled: d.hidppEnabled,
      detected: detected ? { version: { major: d.hidpp![0], minor: d.hidpp![1] } } : undefined,
      status,
    }];
  }

  record(d: FakeDevice) {
    const connected = d.state === "connected";
    const info = Object.entries(d.info)
      .filter(([key]) => connected || !key.startsWith("battery."))
      .map(([key, value]) => ({ key, value: wireValue(typeof value === "boolean" ? "bool" : typeof value === "number" ? "integer" : "text", value) }));
    return create(DeviceSchema, {
      id: d.id,
      transport: TRANSPORT[d.transport],
      name: d.name,
      kind: upper(Kind, d.kind),
      state: upper(DeviceState, d.state),
      enabled: d.enabled,
      trusted: d.trusted,
      blocked: d.blocked,
      paused: d.paused,
      inactive: this.inactive(d),
      error: d.error ?? undefined,
      security: connected ? { encrypted: true, authenticated: false, secureConnections: true, keySize: 16 } : undefined,
      integrations: this.#hidpp(d),
      info,
      roles: d.roles.map((r) => upper(Role, r)),
    });
  }

  #setting(s: FakeSetting) {
    const status = s.saved === null ? undefined : s.error !== null ? { case: "error" as const, value: s.error } : { case: "state" as const, value: upper(SettingState, s.state) };
    const value = s.value ?? undefined;
    const saved = s.saved ?? undefined;
    const big = (v: Scalar | undefined) => (v === undefined ? undefined : BigInt(v as number));
    const type =
      s.type === "bool" ? { case: "bool" as const, value: { value: value as boolean | undefined, saved: saved as boolean | undefined } }
        : s.type === "integer" ? {
            case: "integer" as const,
            value: {
              value: big(value),
              saved: big(saved),
              limits: s.choices.length
                ? { case: "choices" as const, value: { values: s.choices.map((c) => BigInt(c as number)) } }
                : s.min !== null && s.max !== null ? { case: "range" as const, value: { min: BigInt(s.min), max: BigInt(s.max), step: BigInt(s.step ?? 0) } } : undefined,
            },
          }
          : s.type === "enum" ? { case: "enum" as const, value: { value: value as string | undefined, saved: saved as string | undefined, choices: s.choices as string[] } }
            : s.type === "color" ? { case: "color" as const, value: { value: value as number | undefined, saved: saved as number | undefined } }
              : { case: "text" as const, value: { value: value as string | undefined, saved: saved as string | undefined, maxBytes: 64 } };
    return create(SettingSchema, { integration: IntegrationKind.HIDPP, key: s.key, status, type });
  }

  settingsOf(d: FakeDevice) {
    return { device: d.id, settings: d.settings.map((s) => this.#setting(s)) };
  }

  warningsOf(d: FakeDevice) {
    return {
      device: d.id,
      warnings: d.warnings.map((w) => ({
        code: upper(WarningCode, w.code),
        service: w.service,
        reportType: w.reportType ? upper(ReportType, w.reportType) : ReportType.UNKNOWN,
        reportId: w.reportId ?? undefined,
        bitOffset: w.bitOffset ?? undefined,
        usagePage: w.usagePage ?? undefined,
        usage: w.usage ?? undefined,
      })),
    };
  }

  // ---- Simulation controls ----------------------------------------------

  /** Changes a device and reports it as the firmware would. */
  changeDevice(id: string, patch: Partial<FakeDevice>) {
    const d = this.find(id);
    const wasActive = this.#active(d);
    Object.assign(d, patch);
    this.#changed(d, wasActive);
  }

  /** Changes what a device reports; the device event carries it. */
  changeInfo(id: string, values: Record<string, Scalar | null>) {
    const d = this.find(id);
    for (const [key, value] of Object.entries(values)) {
      if (value === null) delete d.info[key];
      else d.info[key] = value;
    }
    this.#event({ case: "device", value: this.record(d) });
  }

  changeWarnings(id: string, warnings: DeviceWarning[]) {
    const d = this.find(id);
    d.warnings = warnings;
    this.#event({ case: "warnings", value: this.warningsOf(d) });
  }

  changeAdapter(patch: { ready?: boolean; storageFull?: boolean }) {
    Object.assign(this, patch);
    this.#event({ case: "adapter", value: this.status() });
  }

  #active(d: FakeDevice) {
    return d.state === "connected" && d.hidppEnabled && d.hidpp !== null && d.hidppState === null && d.hidppError === null;
  }

  /** Reports a changed device, and its settings when HID++ came up or went down. */
  #changed(d: FakeDevice, wasActive: boolean) {
    this.#event({ case: "device", value: this.record(d) });
    if (wasActive === this.#active(d)) return;
    // Values become current, or possibly stale, as HID++ comes up or goes down.
    if (this.#active(d)) this.#apply(d);
    else this.#event({ case: "settings", value: this.settingsOf(d) });
  }

  /** Writes every saved setting to an active device, then reports the list. */
  #apply(d: FakeDevice) {
    const session = this.#session;
    const run = () => {
      if (this.#session !== session || !this.#active(d)) return;
      for (const s of d.settings) {
        if (s.saved === null) continue;
        const error = this.settingFailures[s.key];
        if (error !== undefined) s.error = error;
        else Object.assign(s, { value: s.saved, state: "applied", error: null });
      }
      this.#event({ case: "settings", value: this.settingsOf(d) });
    };
    if (this.#settingJobMs) setTimeout(run, this.#settingJobMs);
    else run();
  }

  // ---- Commands ----------------------------------------------------------

  #handle(request: Request) {
    const command = request.command;
    const failure = command.case ? this.failures[command.case]?.shift() : undefined;
    try {
      if (failure !== undefined) throw new Refusal(failure, failure === ErrorCode.NO_CAPACITY ? CapacityReason.ENABLED : CapacityReason.UNKNOWN);
      this.#run(command);
    } catch (error) {
      if (!(error instanceof Refusal)) throw error;
      this.#reply({ result: { case: "error", value: { code: error.code, reason: error.reason } } });
    }
  }

  #needReady() {
    if (!this.ready) throw new Refusal(ErrorCode.NOT_READY);
  }

  #run(command: Request["command"]) {
    switch (command.case) {
      case "getStatus":
        return this.#reply({ result: { case: "status", value: this.status() } });
      case "setAdapter": {
        const { name, platform } = command.value;
        const next = name === undefined ? this.name : name === "" ? this.defaultName : adapterName(name);
        if (next === null) throw new Refusal(ErrorCode.BAD_ARGS);
        if (platform !== undefined && !(platform in Platform)) throw new Refusal(ErrorCode.BAD_ARGS);
        this.#needReady();
        const changed = next !== this.name || (platform !== undefined && Platform[platform]!.toLowerCase() !== this.platform);
        this.name = next;
        if (platform !== undefined) this.platform = Platform[platform]!.toLowerCase() as FakeAdapter["platform"];
        this.#reply({ result: { case: "status", value: this.status() } });
        if (changed) this.#event({ case: "adapter", value: this.status() });
        return;
      }
      case "startScan": {
        const { transports, seconds } = command.value;
        if (!transports.length || seconds > 60) throw new Refusal(ErrorCode.BAD_ARGS);
        if (transports.some((t) => !this.transports.some((x) => TRANSPORT[x] === t))) throw new Refusal(ErrorCode.UNSUPPORTED);
        this.#needReady();
        // A running scan ends, and reports so, before the new one is answered.
        this.#stopScan(this.candidates.length);
        const found = this.candidates.filter((c) => c.transport && transports.includes(TRANSPORT[c.transport]));
        this.#reply({});
        const session = this.#session;
        const timers = found.map((c, i) =>
          setTimeout(() => {
            if (this.#session === session) this.#event({ case: "scanFound", value: { id: c.id, transport: TRANSPORT[c.transport!], name: c.name, kind: upper(Kind, c.kind), rssi: c.rssi ?? undefined } });
          }, 30 * (i + 1)),
        );
        timers.push(setTimeout(() => this.#stopScan(found.length), (seconds || 10) * 1000));
        this.#scan = timers;
        return;
      }
      case "stopScan":
        this.#reply({});
        this.#stopScan(this.candidates.length);
        return;
      case "startPairing": {
        if (this.#pairing) throw new Refusal(ErrorCode.BUSY);
        const candidate = this.candidates.find((c) => c.id === command.value.candidate);
        if (!candidate) throw new Refusal(ErrorCode.NOT_FOUND);
        this.#needReady();
        if (this.storageFull) throw new Refusal(ErrorCode.NO_CAPACITY, CapacityReason.STORAGE);
        const pairing = { candidate, prompt: false };
        this.#pairing = pairing;
        this.#reply({});
        this.#pairingEvent({ case: "connecting", value: {} });
        const method = this.pairingMethod;
        setTimeout(() => {
          if (this.#pairing !== pairing) return;
          if (!method) return this.#paired();
          pairing.prompt = method !== "show";
          if (method === "confirm") this.#pairingEvent({ case: "confirmCode", value: { passkey: "042731" } });
          else if (method === "enter") this.#pairingEvent({ case: "enterCode", value: { kind: CodeKind.PASSKEY } });
          else {
            this.#pairingEvent({ case: "showCode", value: { kind: CodeKind.PASSKEY, value: "042731" } });
            setTimeout(() => this.#pairing === pairing && this.#paired(), 200);
          }
        }, 20);
        return;
      }
      case "acceptPrompt":
      case "rejectPrompt": {
        const p = this.#pairing;
        if (!p?.prompt) throw new Refusal(ErrorCode.NO_PROMPT);
        p.prompt = false;
        this.#reply({});
        if (command.case === "rejectPrompt") {
          this.#pairing = null;
          this.#pairingEvent({ case: "failed", value: ErrorCode.REJECTED }, p.candidate.id);
        } else setTimeout(() => this.#pairing === p && this.#paired(), 20);
        return;
      }
      case "cancelPairing": {
        const p = this.#pairing;
        this.#pairing = null;
        this.#reply({});
        if (p) this.#pairingEvent({ case: "failed", value: ErrorCode.CANCELLED }, p.candidate.id);
        return;
      }
      case "listDevices":
        return this.#reply({ result: { case: "devices", value: { devices: this.devices.map((d) => this.record(d)) } } });
      case "getDevice":
        return this.#reply({ result: { case: "device", value: this.record(this.find(command.value.device)) } });
      case "setDevice": {
        const d = this.find(command.value.device);
        const { enabled, trusted, blocked, integrations } = command.value;
        const kinds = integrations.map((i) => i.kind);
        if (new Set(kinds).size !== kinds.length) throw new Refusal(ErrorCode.BAD_ARGS);
        if (kinds.some((k) => k !== IntegrationKind.HIDPP)) throw new Refusal(ErrorCode.UNSUPPORTED);
        this.#needReady();
        if (enabled && !d.enabled && !d.blocked) {
          const used = this.devices.filter((x) => x !== d && x.transport === d.transport && this.inactive(x) === undefined).length;
          if (used >= this.maxEnabled) throw new Refusal(ErrorCode.NO_CAPACITY, CapacityReason.ENABLED);
        }
        const patch: Partial<FakeDevice> = {};
        if (enabled !== undefined) patch.enabled = enabled;
        if (trusted !== undefined) patch.trusted = trusted;
        if (blocked !== undefined) patch.blocked = blocked;
        const hidpp = integrations.find((i) => i.enabled !== undefined)?.enabled;
        if (hidpp !== undefined) patch.hidppEnabled = hidpp;
        if ((patch.blocked || patch.enabled === false) && d.state !== "disconnected") patch.state = "disconnected";
        const wasActive = this.#active(d);
        Object.assign(d, patch);
        this.#reply({ result: { case: "device", value: this.record(d) } });
        this.#changed(d, wasActive);
        return;
      }
      case "connectDevice": {
        const d = this.find(command.value.device);
        this.#needReady();
        if (this.#pairing) throw new Refusal(ErrorCode.BUSY);
        const inactive = this.inactive(d);
        if (inactive === InactiveReason.DISABLED) throw new Refusal(ErrorCode.DISABLED);
        if (inactive === InactiveReason.BLOCKED) throw new Refusal(ErrorCode.BLOCKED);
        if (inactive === InactiveReason.CAPACITY) throw new Refusal(ErrorCode.NO_CAPACITY, CapacityReason.ENABLED);
        if (inactive === InactiveReason.UNSUPPORTED_TRANSPORT) throw new Refusal(ErrorCode.UNSUPPORTED);
        if (d.state === "connected") return this.#reply({ result: { case: "device", value: this.record(d) } });
        Object.assign(d, { state: "connecting", paused: false, error: null });
        this.#reply({ result: { case: "device", value: this.record(d) } });
        this.#event({ case: "device", value: this.record(d) });
        const session = this.#session;
        setTimeout(() => {
          if (this.#session === session && d.state === "connecting")
            this.changeDevice(d.id, { state: "connected", roles: d.roles.length ? d.roles : ["keyboard"] });
        }, 20);
        return;
      }
      case "disconnectDevice": {
        const d = this.find(command.value.device);
        const wasActive = this.#active(d);
        Object.assign(d, { state: "disconnected", paused: true });
        this.#reply({ result: { case: "device", value: this.record(d) } });
        this.#changed(d, wasActive);
        return;
      }
      case "unpairDevice": {
        const d = this.find(command.value.device);
        this.#needReady();
        this.devices = this.devices.filter((x) => x !== d);
        this.#reply({});
        this.#event({ case: "deviceRemoved", value: { id: d.id } });
        return;
      }
      case "refreshDevice": {
        const d = this.find(command.value.device);
        if (d.state !== "connected") throw new Refusal(ErrorCode.NOT_CONNECTED);
        this.#reply({});
        this.#event({ case: "device", value: this.record(d) });
        this.#event({ case: "settings", value: this.settingsOf(d) });
        return;
      }
      case "listWarnings":
        return this.#reply({ result: { case: "warnings", value: this.warningsOf(this.find(command.value.device)) } });
      case "listSettings":
        return this.#reply({ result: { case: "settings", value: this.settingsOf(this.find(command.value.device)) } });
      case "setSettings":
      case "forgetSettings": {
        const d = this.find(command.value.device);
        const refs = command.case === "setSettings" ? command.value.changes : command.value.settings;
        if (!refs.length || new Set(refs.map((r) => r.key)).size !== refs.length) throw new Refusal(ErrorCode.BAD_ARGS);
        const rows = refs.map((r) => {
          const s = d.settings.find((x) => x.key === r.key);
          if (!s || r.integration !== IntegrationKind.HIDPP) throw new Refusal(ErrorCode.NOT_FOUND);
          return s;
        });
        const values = command.case === "setSettings" ? command.value.changes.map((c, i) => fromWire(rows[i]!.type, c.value)) : [];
        if (values.some((v) => v === undefined)) throw new Refusal(ErrorCode.BAD_ARGS);
        this.#needReady();
        rows.forEach((s, i) => {
          if (command.case === "setSettings") Object.assign(s, { saved: values[i], state: "pending", error: null });
          else Object.assign(s, { saved: null, state: "pending", error: null });
        });
        this.#reply({ result: { case: "settings", value: this.settingsOf(d) } });
        if (command.case === "setSettings") this.#apply(d);
        return;
      }
      case "listFeatures":
        this.find(command.value.device);
        return this.#reply({ result: { case: "features", value: { features: [] } } });
      case "listFiles":
        return this.#reply({ result: { case: "files", value: { entries: [] } } });
      default:
        throw new Refusal(ErrorCode.UNKNOWN_COMMAND);
    }
  }

  #stopScan(count: number) {
    if (!this.#scan) return;
    for (const t of this.#scan) clearTimeout(t);
    this.#scan = null;
    this.#event({ case: "scanDone", value: { count, truncated: false } });
  }

  #pairingEvent(step: MessageInitShape<typeof PairingSchema>["step"], candidate = this.#pairing?.candidate.id ?? "") {
    this.#event({ case: "pairing", value: { candidate, step } });
  }

  #paired() {
    const p = this.#pairing;
    if (!p) return;
    this.#pairing = null;
    const created = device(`d_${this.#nextDevice++}`, {
      name: p.candidate.name,
      kind: p.candidate.kind,
      transport: p.candidate.transport ?? "ble",
      state: "connecting",
      info: { "battery.level": 90 },
    });
    this.devices.push(created);
    this.#event({ case: "device", value: this.record(created) });
    this.#pairingEvent({ case: "done", value: { device: created.id } }, p.candidate.id);
    const session = this.#session;
    setTimeout(() => {
      if (this.#session === session && created.state === "connecting") this.changeDevice(created.id, { state: "connected", roles: ["keyboard"] });
    }, 20);
  }
}
