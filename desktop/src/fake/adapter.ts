// A simulated adapter speaking the serial API over an in-memory ByteStream,
// following the firmware's behavior: every command answers at once, and
// changes follow as events after the response. Listings come in pages of
// `pageSize` entries, and settings, warnings and rules events carry only what
// changed since the client last listed or heard of them. Tests and the demo
// mode use it; all data is synthetic.
import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import { compareSettingRefs, compareUsages, compareWarnings, ruleInput, settingRef, type ByteStream } from "@cordial/client";
import {
  CapacityReason,
  CodeKind,
  ConfigurationInterface,
  DELIMITER,
  DeviceSchema,
  DeviceState,
  DeviceWarningSchema,
  ErrorCode,
  FrameDecoder,
  InactiveReason,
  IntegrationKind,
  IntegrationState,
  Kind,
  MessageSchema,
  Platform,
  ProfileRuleSchema,
  ReportType,
  RequestSchema,
  Role,
  SettingSchema,
  SettingState,
  StatusSchema,
  Transport,
  WarningCode,
  encodeFrame,
  type DeviceWarning as WireWarning,
  type EventSchema,
  type PairingSchema,
  type ProfileRule,
  type Request,
  type Setting,
  type ResponseSchema,
  type Usage,
  type Value,
  type ValueSchema,
} from "@cordial/protocol";
import { adapterName } from "../shared/adapter-name.ts";
import type { Candidate, DeviceWarning, KindName, RoleName, Scalar, ValueType } from "../shared/state.ts";

type MessageInit = MessageInitShape<typeof MessageSchema>;
type ValueInit = MessageInitShape<typeof ValueSchema>;
type Command<C extends Request["command"]["case"]> = Extract<Request["command"], { case: C }>["value"];

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
  id: number;
  transport: "classic" | "ble";
  name: string;
  kinds: KindName[];
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
  roles: RoleName[];
  /** What the device reports while connected, by catalog key. */
  info: Record<string, Scalar>;
  /** Settings as read on the current connection, with the saved values. */
  settings: FakeSetting[];
  /** Warnings on the current connection. */
  warnings: DeviceWarning[];
  /** The device's layers: profile IDs in the order they apply. */
  profiles: number[];
  /** Why the connected device's profiles aren't loaded; the fake works it out. */
  profileError: ErrorCode | null;
}

/** A saved profile with its rules. */
export interface FakeProfile {
  id: number;
  name: string;
  rules: ProfileRule[];
  /** Bytes it takes once loaded, in place of the size its rules give it. */
  size?: number;
}

export type InterfaceName = "via" | "vial";

export interface FakeOptions {
  adapterId?: string;
  board?: string;
  /** Transports the firmware supports. */
  transports?: ("classic" | "ble")[];
  /** Which supported transports are enabled; as in the firmware, Classic starts disabled and BLE
   * enabled. */
  enabled?: Partial<Record<"classic" | "ble", boolean>>;
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
  /** Whether the board has profiles; every board but the Pico W does. */
  profileSupport?: boolean;
  profiles?: FakeProfile[];
  /** Saved configuration interface preferences. */
  interfaces?: Partial<Record<InterfaceName, { enabled: boolean; profile: number }>>;
  /** Bytes for loaded profiles. */
  memoryBudget?: number;
  /** Profiles per device layer list. */
  maxLayers?: number;
  /** Entries per listed page. */
  pageSize?: number;
}

/** The memory in use and each device's profile error, before a change that may load profiles. */
interface Loads {
  used: number;
  errors: Map<number, ErrorCode | null>;
}

/** Bytes a loaded profile takes, and each of its rules. */
const PROFILE_BYTES = 64;
const RULE_BYTES = 16;

const usage = (usagePage: number, u: number) => ({ usagePage, usage: u });

/** A remap rule from one HID usage to others. */
export function remap(input: [number, number], outputs: [number, number][]): ProfileRule {
  return create(ProfileRuleSchema, {
    input: usage(...input),
    effect: { case: "remap", value: { outputs: outputs.map((o) => ({ usage: usage(...o) })) } },
  });
}

/** A scale rule on a relative HID usage. */
export function scale(input: [number, number], numerator: number, denominator = 1): ProfileRule {
  return create(ProfileRuleSchema, { input: usage(...input), effect: { case: "scale", value: { numerator, denominator } } });
}

/** One rule giving each role, for tests and demos. */
const ROLE_RULES: Record<RoleName, ProfileRule> = {
  keyboard: remap([0x07, 0x39], [[0x07, 0x29]]),
  mouse: scale([0x01, 0x38], -1),
  consumer_control: remap([0x0c, 0xe9], [[0x0c, 0xea]]),
  system_control: remap([0x01, 0x82], []),
};

/** A profile whose rules give it `roles`, for tests and demos. */
export function profile(id: number, name: string, roles: RoleName[] = [], size?: number): FakeProfile {
  return { id, name, rules: roles.map((r) => ROLE_RULES[r]), ...(size === undefined ? {} : { size }) };
}

/** The role one rule adds, from its input. */
function ruleRole(rule: ProfileRule): RoleName | null {
  const { usagePage: page, usage: u } = rule.input ?? { usagePage: 0, usage: 0 };
  if (page === 0x07) return "keyboard";
  if (page === 0x09 || (page === 0x01 && [0x30, 0x31, 0x38].includes(u)) || (page === 0x0c && u === 0x238)) return "mouse";
  if (page === 0x0c) return "consumer_control";
  if (page === 0x01 && u >= 0x81 && u <= 0xb7) return "system_control";
  return null;
}

const ROLE_ORDER: RoleName[] = ["keyboard", "mouse", "consumer_control", "system_control"];
const rolesOf = (p: FakeProfile) => ROLE_ORDER.filter((r) => p.rules.some((rule) => ruleRole(rule) === r));
const sizeOf = (p: FakeProfile) => p.size ?? PROFILE_BYTES + RULE_BYTES * p.rules.length;
const sameUsage = (a: Usage | undefined, b: Usage | undefined) => a?.usagePage === b?.usagePage && a?.usage === b?.usage;
const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
const encodedRule = (r: ProfileRule) => hex(toBinary(ProfileRuleSchema, r));

/** The page of `list`, sorted by `compare`, whose keys follow `after`, and whether it ends the list. */
function page<E, K>(list: E[], after: K | undefined, size: number, key: (e: E) => K, compare: (a: K, b: K) => number) {
  const later = list.filter((e) => after === undefined || compare(key(e), after) > 0).sort((a, b) => compare(key(a), key(b)));
  const entries = later.slice(0, size);
  return { entries, end: entries.length === later.length };
}

export function device(id: number, patch: Partial<FakeDevice> = {}): FakeDevice {
  return {
    id,
    transport: "ble",
    name: "Keyboard",
    kinds: ["keyboard"],
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
    profiles: [],
    profileError: null,
    ...patch,
  };
}

export function setting(key: string, patch: Partial<FakeSetting> = {}): FakeSetting {
  return { key, type: "bool", value: null, saved: null, state: "pending", error: null, choices: [], min: null, max: null, step: null, ...patch };
}

/** A few synthetic devices showing the main states. */
export function demoDevices(): FakeDevice[] {
  return [
    device(1, {
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
        setting("keyboard.platform", { type: "enum", choices: ["windows", "linux", "chrome_os", "android", "mac", "ios"], value: "linux" }),
        setting("backlight.enabled", { value: true, saved: true, state: "applied" }),
        setting("backlight.mode", { type: "enum", choices: ["automatic", "permanent_manual"], value: "automatic" }),
        setting("backlight.level", { type: "integer", min: 0, max: 7, step: 1, value: 3 }),
        setting("backlight.delay.hands_out", { type: "integer", min: 5, max: 7200, step: 5, value: 60, saved: 30, state: "changed_on_device" }),
        setting("power.auto_off", { type: "integer", min: 0, max: 15300, step: 60, value: 1800 }),
      ],
    }),
    device(2, {
      name: "Example Mouse",
      kinds: ["mouse"],
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
    device(3, { name: "Travel Keyboard", transport: "classic", info: { "firmware.version": "2.1" } }),
    device(4, { name: "Old Mouse", kinds: ["mouse"], enabled: false }),
  ];
}

/** Synthetic profiles for the demo devices; the first device's layers use the first, and the
 * second device's the second. */
export function demoProfiles(): FakeProfile[] {
  return [profile(1, "Typing", ["keyboard", "consumer_control"]), profile(2, "Natural Scrolling", ["mouse"]), profile(3, "Empty")];
}

/** A second, BLE-only set of synthetic devices, so a second simulated adapter is distinguishable. */
export function demoDevicesBle(): FakeDevice[] {
  return [
    device(1, {
      name: "Example Compact Keyboard",
      state: "connected",
      roles: ["keyboard", "consumer_control"],
      info: { "device.manufacturer": "Example Co", "battery.level": 54, "battery.charging": true },
    }),
    device(2, { name: "Example Trackball", kinds: ["mouse"], roles: ["mouse"] }),
  ];
}

export function demoCandidatesBle(): Candidate[] {
  return [
    { id: 1, kinds: ["keyboard"], name: "Example Numpad", transport: "ble", rssi: -55 },
    { id: 2, kinds: [], name: "", transport: "ble", rssi: -78 },
  ];
}

export function demoCandidates(): Candidate[] {
  return [
    { id: 1, kinds: ["keyboard"], name: "Example Keys Mini", transport: "ble", rssi: -48 },
    { id: 2, kinds: ["mouse"], name: "Example Pebble", transport: "ble", rssi: -63 },
    { id: 3, kinds: ["keyboard"], name: "Example Classic Keyboard", transport: "classic", rssi: -71 },
    { id: 4, kinds: [], name: "", transport: "ble", rssi: -80 },
  ];
}

const BOARDS: Record<string, string> = { pico_w: "Pico W", pico2_w: "Pico 2 W", xiao_esp32s3: "XIAO ESP32-S3", waveshare_rp2350b_plus_w: "RP2350B-Plus-W" };
const TRANSPORT = { classic: Transport.CLASSIC, ble: Transport.BLE } as const;
const INTERFACE = { via: ConfigurationInterface.VIA, vial: ConfigurationInterface.VIAL } as const;
/** The marker Vial looks for after the adapter ID in the USB serial number. */
const VIAL_SERIAL = "-vial:f64c2b3c";
const upper = <E>(e: E, name: string) => (e as Record<string, number>)[name.toUpperCase()]!;

function wireValue(type: ValueType, v: Scalar): ValueInit {
  if (type === "bool") return { value: { case: "bool", value: v as boolean } };
  if (type === "integer") return { value: { case: "integer", value: BigInt(v as number) } };
  if (type === "color") return { value: { case: "color", value: v as number } };
  return { value: { case: "text", value: String(v) } };
}

/** A wire value as the setting takes it, or undefined when its type or limits don't allow it. */
function fromWire(s: FakeSetting, v: Value | undefined): Scalar | undefined {
  const value = v?.value;
  let out: Scalar | undefined;
  if (s.type === "bool" && value?.case === "bool") out = value.value;
  else if (s.type === "integer" && value?.case === "integer") out = Number(value.value);
  else if (s.type === "color" && value?.case === "color") out = value.value;
  else if ((s.type === "enum" || s.type === "text") && value?.case === "text") out = value.value;
  if (out === undefined) return undefined;
  if (s.choices.length && !s.choices.includes(out)) return undefined;
  if (typeof out === "number" && ((s.min !== null && out < s.min) || (s.max !== null && out > s.max))) return undefined;
  return out;
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
  enabled: Record<"classic" | "ble", boolean>;
  platform: "linux" | "windows" | "mac" = "linux";
  name: string;
  readonly defaultName: string;
  ready: boolean;
  storageFull = false;
  maxEnabled: number;
  /** Requests received in this session, for assertions. */
  received: Request[] = [];
  /** Events sent in this session, for assertions. */
  events: NonNullable<MessageInitShape<typeof EventSchema>["kind"]>[] = [];
  /** Error codes to answer the next requests of a command with, in order, by command case. */
  failures: Record<string, ErrorCode[]> = {};
  /** Whether a STORAGE_FAILED failure leaves the outcome of its write unknown. */
  uncertainWrites = false;
  /** Apply failures for settings that are saved anyway. */
  settingFailures: Record<string, ErrorCode> = {};
  readonly profileSupport: boolean;
  profiles: FakeProfile[];
  /** Saved configuration interface preferences, in ConfigurationInterface order. */
  interfaces: Record<InterfaceName, { enabled: boolean; profile: number }>;
  memoryBudget: number;
  maxLayers: number;
  pageSize: number;
  /** Records whose flash read fails: listed as unreadable, and a profile here can't load. */
  unreadableDevices = new Set<number>();
  unreadableProfiles = new Set<number>();
  /** Records listed as entries of a kind newer than the client, which it reads as unset. */
  newerDevices = new Set<number>();
  newerProfiles = new Set<number>();
  /** How often USB reconnected for a configuration interface change. */
  usbReconnects = 0;
  /** Whether the USB device is attached; false briefly while it reconnects. */
  present = true;
  /** Bytes from an earlier session delivered right after the next open. */
  staleInput: Uint8Array | null = null;
  /** Called when the USB device is attached again after reconnecting. */
  readonly #hotplug: (() => void)[] = [];
  /** Profiles loaded for each connected device, by device ID. */
  readonly #loaded = new Map<number, number[]>();
  #nextProfile: number;
  readonly #latency: number;
  readonly #settingJobMs: number;
  #data: ((chunk: Uint8Array) => void)[] = [];
  #close: ((error: Error | null) => void)[] = [];
  #decoder = new FrameDecoder(1024);
  #open = false;
  #starting = false;
  #session = 0;
  #scan: ReturnType<typeof setTimeout>[] | null = null;
  /** The transports the running scan still covers. */
  #scanning: ("classic" | "ble")[] = [];
  #pairing: { candidate: Candidate; prompt: boolean } | null = null;
  #nextDevice: number;
  /** Replies and events waiting for the reply delay, in order. */
  #outbox: Promise<void> = Promise.resolve();
  #gate: Promise<void> | null = null;
  /** The settings and warnings of each device this session's client has listed or heard of, by
   * their encoding, so events carry only what changed since. */
  readonly #seenSettings = new Map<number, Map<string, string>>();
  readonly #seenWarnings = new Map<number, Map<string, WireWarning>>();

  constructor(options: FakeOptions = {}) {
    this.id = options.adapterId ?? "0000000000000F01";
    this.board = options.board ?? "pico2_w";
    this.name = BOARDS[this.board] ?? this.board;
    this.defaultName = this.name;
    this.devices = options.devices ?? demoDevices();
    this.candidates = options.candidates ?? demoCandidates();
    this.pairingMethod = options.pairingMethod === undefined ? "confirm" : options.pairingMethod;
    this.transports = options.transports ?? ["classic", "ble"];
    this.enabled = { classic: false, ble: true, ...options.enabled };
    this.ready = options.ready ?? true;
    this.maxEnabled = options.maxEnabled ?? 7;
    this.#latency = options.latency ?? 0;
    this.#settingJobMs = options.settingJobMs ?? 0;
    this.profileSupport = options.profileSupport ?? this.board !== "pico_w";
    this.profiles = this.profileSupport ? (options.profiles ?? []) : [];
    if (!this.profileSupport) for (const d of this.devices) d.profiles = [];
    this.interfaces = {
      via: { enabled: false, profile: 0, ...options.interfaces?.via },
      vial: { enabled: false, profile: 0, ...options.interfaces?.vial },
    };
    this.memoryBudget = options.memoryBudget ?? 4096;
    this.maxLayers = options.maxLayers ?? 4;
    this.pageSize = options.pageSize ?? 8;
    this.#nextProfile = Math.max(0, ...this.profiles.map((p) => p.id)) + 1;
    this.#nextDevice = Math.max(9, ...this.devices.map((d) => d.id)) + 1;
    this.#load(null);
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
    if (!this.present) throw new Error("no such device");
    this.#end();
    this.#open = true;
    this.#session++;
    this.received = [];
    this.events = [];
    this.#seenSettings.clear();
    this.#seenWarnings.clear();
    this.#starting = true;
    return this;
  }

  /** Simulates unplugging: the port fails. */
  unplug() {
    this.#end();
    for (const listener of this.#close.splice(0)) listener(new Error("device disconnected"));
  }

  /** The USB serial number: the adapter ID, followed by Vial's marker while Vial is enabled. */
  get serial() {
    return this.interfaces.vial.enabled ? `${this.id}${VIAL_SERIAL}` : this.id;
  }

  /** Calls `listener` whenever the USB device is attached again after reconnecting. */
  onHotplug(listener: () => void) {
    this.#hotplug.push(listener);
  }

  /** The session ends: a scan stops and an unsaved pairing is cancelled. */
  #end() {
    this.#open = false;
    for (const t of this.#scan ?? []) clearTimeout(t);
    this.#scan = null;
    this.#pairing = null;
    this.#decoder = new FrameDecoder(1024);
  }

  /** Reconnects USB once everything already written has gone out: the session ends, and the
   * device is attached again shortly after with its new interfaces. */
  #reconnectUsb() {
    this.usbReconnects++;
    const session = this.#session;
    this.#outbox = this.#outbox.then(() => {
      if (this.#session !== session) return;
      this.present = false;
      this.unplug();
      setTimeout(() => {
        this.present = true;
        for (const listener of this.#hotplug) listener();
      }, 100);
    });
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

  #event(event: NonNullable<MessageInitShape<typeof EventSchema>["kind"]>) {
    this.events.push(event);
    this.#message({ kind: { case: "event", value: { kind: event } } });
  }

  // ---- Model ----------------------------------------------------------------

  find(id: number): FakeDevice {
    const d = this.devices.find((x) => x.id === id);
    if (!d) throw new Refusal(ErrorCode.NOT_FOUND);
    return d;
  }

  /** The transports in use: those supported and enabled. */
  available(): ("classic" | "ble")[] {
    return this.transports.filter((t) => this.enabled[t]);
  }

  /** Bytes of the budget the loaded profiles take; a profile several devices use counts once. */
  memoryUsed(loaded = this.#loaded) {
    const ids = new Set([...loaded.values()].flat());
    return this.profiles.filter((p) => ids.has(p.id)).reduce((n, p) => n + sizeOf(p), 0);
  }

  status() {
    const info: { key: string; value: ValueInit }[] = [
      { key: "firmware.version", value: wireValue("text", "0.0.0-dev") },
      { key: "board.name", value: wireValue("text", this.board) },
      { key: "build.development", value: wireValue("bool", true) },
    ];
    if (this.storageFull) info.push({ key: "storage.full", value: wireValue("bool", true) });
    return create(StatusSchema, {
      id: this.id,
      name: this.name,
      platform: upper(Platform, this.platform),
      ready: this.ready,
      transports: this.transports.map((t) => ({ transport: TRANSPORT[t], maxEnabled: this.maxEnabled, enabled: this.enabled[t] })),
      info,
      ...(this.profileSupport
        ? {
            profileSupport: {
              remapInputs: [
                { usagePage: 0x07, min: 0x04, max: 0xa4 },
                { usagePage: 0x07, min: 0xe0, max: 0xe7 },
                { usagePage: 0x09, min: 0x01, max: 0x10 },
                { usagePage: 0x0c, min: 0x01, max: 0x29c },
                { usagePage: 0x01, min: 0x81, max: 0xb7 },
              ],
              scaleInputs: [
                { usagePage: 0x01, min: 0x30, max: 0x31 },
                { usagePage: 0x01, min: 0x38, max: 0x38 },
                { usagePage: 0x0c, min: 0x238, max: 0x238 },
              ],
              remapOutputs: [
                { collection: usage(0x01, 0x06), usagePage: 0x07, min: 0x04, max: 0xe7 },
                { collection: usage(0x01, 0x02), usagePage: 0x09, min: 0x01, max: 0x05 },
                { collection: usage(0x0c, 0x01), usagePage: 0x0c, min: 0x01, max: 0x29c },
              ],
              memoryBudget: this.memoryBudget,
              maxRemapOutputs: 8,
              maxLayers: this.maxLayers,
              memoryUsed: this.memoryUsed(),
            },
            configurationInterfaces: (["via", "vial"] as const).map((i) => ({
              interface: INTERFACE[i],
              enabled: this.interfaces[i].enabled,
              profile: this.interfaces[i].profile,
              conflicts: [INTERFACE[i === "via" ? "vial" : "via"]],
            })),
          }
        : {}),
    });
  }

  /** Why the adapter doesn't use `d`, as the firmware decides it: the first enabled devices fill each transport's places. */
  inactive(d: FakeDevice): InactiveReason | undefined {
    if (!this.transports.includes(d.transport)) return InactiveReason.UNSUPPORTED_TRANSPORT;
    if (!this.available().includes(d.transport)) return InactiveReason.TRANSPORT_DISABLED;
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
    // Information belongs to the current connection.
    const info = connected
      ? Object.entries(d.info).map(([key, value]) => ({ key, value: wireValue(typeof value === "boolean" ? "bool" : typeof value === "number" ? "integer" : "text", value) }))
      : [];
    return create(DeviceSchema, {
      id: d.id,
      transport: TRANSPORT[d.transport],
      name: d.name,
      kinds: d.kinds.map((k) => upper(Kind, k)),
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
      profiles: this.profileSupport ? { profiles: d.profiles } : undefined,
      profileError: connected ? (d.profileError ?? undefined) : undefined,
    });
  }

  #setting(s: FakeSetting, connected: boolean) {
    const status = s.saved === null ? undefined : s.error !== null ? { case: "error" as const, value: s.error } : { case: "state" as const, value: upper(SettingState, s.state) };
    // A disconnected device's values haven't been read on this connection.
    const value = connected ? (s.value ?? undefined) : undefined;
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

  /** The settings the adapter knows, in listing order: those read on the current connection and
   * every saved value. */
  settingsOf(d: FakeDevice): Setting[] {
    const connected = d.state === "connected";
    const known = connected ? d.settings : d.settings.filter((s) => s.saved !== null);
    return known.map((s) => this.#setting(s, connected)).sort((a, b) => compareSettingRefs(settingRef(a), settingRef(b)));
  }

  /** The current connection's warnings in listing order; none while disconnected. */
  warningsOf(d: FakeDevice): WireWarning[] {
    return (d.state === "connected" ? d.warnings : []).map((w) => create(DeviceWarningSchema, {
      code: upper(WarningCode, w.code),
      service: w.service,
      reportType: w.reportType ? upper(ReportType, w.reportType) : ReportType.UNKNOWN,
      reportId: w.reportId ?? undefined,
      bitOffset: w.bitOffset ?? undefined,
      usagePage: w.usagePage ?? undefined,
      usage: w.usage ?? undefined,
    })).sort(compareWarnings);
  }

  /** Reports the settings of `d` that changed, appeared or went away since the client last
   * listed or heard of them. */
  #settingsChanged(d: FakeDevice) {
    const seen = this.#seenSettings.get(d.id) ?? new Map<string, string>();
    const current = this.settingsOf(d);
    const now = new Map(current.map((s) => [s.key, hex(toBinary(SettingSchema, s))]));
    const changed = current.filter((s) => seen.get(s.key) !== now.get(s.key));
    const removed = [...seen.keys()].filter((k) => !now.has(k)).map((key) => ({ integration: IntegrationKind.HIDPP, key }));
    this.#seenSettings.set(d.id, now);
    if (changed.length || removed.length) this.#event({ case: "settingsChanged", value: { device: d.id, changed, removed } });
  }

  /** Reports the warnings of `d` added or removed since the client last listed or heard of them. */
  #warningsChanged(d: FakeDevice) {
    const seen = this.#seenWarnings.get(d.id) ?? new Map<string, WireWarning>();
    const now = new Map(this.warningsOf(d).map((w) => [hex(toBinary(DeviceWarningSchema, w)), w]));
    const added = [...now].filter(([k]) => !seen.has(k)).map(([, w]) => w);
    const removed = [...seen].filter(([k]) => !now.has(k)).map(([, w]) => w);
    this.#seenWarnings.set(d.id, now);
    if (added.length || removed.length) this.#event({ case: "warningsChanged", value: { device: d.id, added, removed } });
  }

  #profileRecord(p: FakeProfile) {
    return { id: p.id, name: p.name, roles: rolesOf(p).map((r) => upper(Role, r)) };
  }

  // ---- Simulation controls ----------------------------------------------

  /** Changes a device and reports it as the firmware would. */
  changeDevice(id: number, patch: Partial<FakeDevice>) {
    const d = this.find(id);
    const before = this.#snapshot(d);
    Object.assign(d, patch);
    this.#changed(d, before);
  }

  /** Changes what a device reports; the device event carries it. */
  changeInfo(id: number, values: Record<string, Scalar | null>) {
    const d = this.find(id);
    for (const [key, value] of Object.entries(values)) {
      if (value === null) delete d.info[key];
      else d.info[key] = value;
    }
    this.#event({ case: "device", value: this.record(d) });
  }

  changeWarnings(id: number, warnings: DeviceWarning[]) {
    const d = this.find(id);
    d.warnings = warnings;
    this.#warningsChanged(d);
  }

  changeAdapter(patch: { ready?: boolean; storageFull?: boolean; memoryBudget?: number }) {
    Object.assign(this, patch);
    this.#load(null);
    this.#event({ case: "adapter", value: this.status() });
  }

  #active(d: FakeDevice) {
    return d.state === "connected" && d.hidppEnabled && d.hidpp !== null && d.hidppState === null && d.hidppError === null;
  }

  #snapshot(d: FakeDevice) {
    return { active: this.#active(d), connected: d.state === "connected" };
  }

  /** Reports a changed device, its profiles loading or unloading, and its settings and warnings
   * when its connection or HID++ came up or went down. `loads` is what was loaded before the change. */
  #changed(d: FakeDevice, before: { active: boolean; connected: boolean }, loads = this.#loads()) {
    this.#load(loads, d);
    this.#event({ case: "device", value: this.record(d) });
    const connected = d.state === "connected";
    if (before.connected !== connected) this.#warningsChanged(d);
    // Values become current, or possibly stale, as HID++ comes up or goes down.
    if (this.#active(d) && !before.active) this.#apply(d);
    else if (before.active !== this.#active(d) || before.connected !== connected) this.#settingsChanged(d);
  }

  /** The memory in use and each device's profile error, to report what loading changes. */
  #loads(): Loads {
    return { used: this.memoryUsed(), errors: new Map(this.devices.map((d) => [d.id, d.profileError])) };
  }

  /** Loads each connected device's layers, all or none: a device whose new profiles don't fit in
   * what is left of the budget, or one of which can't be read, loads none of them. A device whose
   * layers changed releases its old profiles first, and one that loaded none tries again. With
   * `report`, the devices whose `profileError` changed since it, other than `except`, are reported,
   * and the adapter when the memory in use changed. */
  #load(report: Loads | null = this.#loads(), except?: FakeDevice) {
    for (const id of [...this.#loaded.keys()])
      if (this.devices.find((d) => d.id === id)?.state !== "connected") this.#loaded.delete(id);
    for (const d of [...this.devices].sort((a, b) => a.id - b.id)) {
      if (d.state !== "connected" || !this.profileSupport) {
        d.profileError = null;
        continue;
      }
      const want = [...new Set(d.profiles)].filter((id) => this.profiles.some((p) => p.id === id));
      const have = this.#loaded.get(d.id);
      if (have && have.join() === want.join()) continue;
      this.#loaded.delete(d.id);
      if (want.some((id) => this.unreadableProfiles.has(id))) {
        d.profileError = ErrorCode.STORAGE_FAILED;
        continue;
      }
      const next = new Map(this.#loaded).set(d.id, want);
      if (this.memoryUsed(next) > this.memoryBudget) d.profileError = ErrorCode.NO_CAPACITY;
      else {
        this.#loaded.set(d.id, want);
        d.profileError = null;
      }
    }
    if (!report) return;
    for (const d of this.devices)
      if (d !== except && report.errors.get(d.id) !== d.profileError) this.#event({ case: "device", value: this.record(d) });
    if (report.used !== this.memoryUsed()) this.#event({ case: "adapter", value: this.status() });
  }

  /** Loads profiles again after a profile changed, as if each device using it reconnected its
   * layers. */
  #reloadUsers(id: number, loads: Loads) {
    for (const [device, ids] of this.#loaded) if (ids.includes(id)) this.#loaded.delete(device);
    this.#load(loads);
  }

  /** Writes every saved setting to an active device, then reports what changed. */
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
      this.#settingsChanged(d);
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
      const outcomeUnknown = this.uncertainWrites && error.code === ErrorCode.STORAGE_FAILED;
      this.#reply({ result: { case: "error", value: { code: error.code, reason: error.reason, outcomeUnknown } } });
    }
  }

  #needReady() {
    if (!this.ready) throw new Refusal(ErrorCode.NOT_READY);
  }

  #needProfiles() {
    if (!this.profileSupport) throw new Refusal(ErrorCode.UNKNOWN_COMMAND);
  }

  #run(command: Request["command"]) {
    switch (command.case) {
      case "getStatus":
        return this.#reply({ result: { case: "status", value: this.status() } });
      case "setAdapter":
        return this.#setAdapter(command.value);
      case "startScan": {
        const { transports, seconds } = command.value;
        if (!transports.length || seconds > 60) throw new Refusal(ErrorCode.BAD_ARGS);
        // Transports that are unsupported or disabled are left out.
        const scanned = this.available().filter((t) => transports.includes(TRANSPORT[t]));
        if (!scanned.length) throw new Refusal(ErrorCode.UNSUPPORTED);
        this.#needReady();
        // A running scan ends, and reports so, before the new one is answered.
        this.#stopScan(this.candidates.length);
        const found = this.candidates.filter((c) => c.transport && scanned.includes(c.transport));
        this.#reply({});
        const session = this.#session;
        this.#scanning = scanned;
        const timers = found.map((c, i) =>
          setTimeout(() => {
            if (this.#session === session && this.#scanning.includes(c.transport!))
              this.#event({ case: "scanFound", value: { id: c.id, transport: TRANSPORT[c.transport!], name: c.name, kinds: c.kinds.map((k) => upper(Kind, k)), rssi: c.rssi ?? undefined } });
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
        if (!this.available().includes(candidate.transport ?? "ble")) throw new Refusal(ErrorCode.UNSUPPORTED);
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
      case "listDevices": {
        const { entries, end } = page(this.devices, command.value.after, this.pageSize, (d) => d.id, (a, b) => a - b);
        const listed = entries.map((d) =>
          this.newerDevices.has(d.id) ? { entry: { case: undefined } }
            : this.unreadableDevices.has(d.id) ? { entry: { case: "unreadable" as const, value: d.id } } : { entry: { case: "device" as const, value: this.record(d) } });
        return this.#reply({ result: { case: "devices", value: { entries: listed, end } } });
      }
      case "getDevice": {
        const d = this.find(command.value.device);
        if (this.unreadableDevices.has(d.id)) throw new Refusal(ErrorCode.STORAGE_FAILED);
        return this.#reply({ result: { case: "device", value: this.record(d) } });
      }
      case "setDevice":
        return this.#setDevice(command.value);
      case "connectDevice": {
        const d = this.find(command.value.device);
        this.#needReady();
        if (this.#pairing) throw new Refusal(ErrorCode.BUSY);
        const inactive = this.inactive(d);
        if (inactive === InactiveReason.DISABLED) throw new Refusal(ErrorCode.DISABLED);
        if (inactive === InactiveReason.BLOCKED) throw new Refusal(ErrorCode.BLOCKED);
        if (inactive === InactiveReason.CAPACITY) throw new Refusal(ErrorCode.NO_CAPACITY, CapacityReason.ENABLED);
        if (inactive === InactiveReason.UNSUPPORTED_TRANSPORT || inactive === InactiveReason.TRANSPORT_DISABLED) throw new Refusal(ErrorCode.UNSUPPORTED);
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
        const before = this.#snapshot(d);
        Object.assign(d, { state: "disconnected", paused: true });
        this.#reply({ result: { case: "device", value: this.record(d) } });
        this.#changed(d, before);
        return;
      }
      case "unpairDevice": {
        const d = this.find(command.value.device);
        this.#needReady();
        this.devices = this.devices.filter((x) => x !== d);
        this.#reply({});
        this.#event({ case: "deviceRemoved", value: { id: d.id } });
        this.#load();
        return;
      }
      case "refreshDevice": {
        const d = this.find(command.value.device);
        if (d.state !== "connected") throw new Refusal(ErrorCode.NOT_CONNECTED);
        this.#reply({});
        this.#event({ case: "device", value: this.record(d) });
        this.#settingsChanged(d);
        return;
      }
      case "listWarnings": {
        const d = this.find(command.value.device);
        const { entries, end } = page(this.warningsOf(d), command.value.after, this.pageSize, (w) => w, compareWarnings);
        const seen = this.#seenWarnings.get(d.id) ?? new Map<string, WireWarning>();
        for (const w of entries) seen.set(hex(toBinary(DeviceWarningSchema, w)), w);
        this.#seenWarnings.set(d.id, seen);
        return this.#reply({ result: { case: "warnings", value: { device: d.id, warnings: entries, end } } });
      }
      case "listSettings": {
        const d = this.find(command.value.device);
        const { entries, end } = page(this.settingsOf(d), command.value.after, this.pageSize, settingRef, compareSettingRefs);
        const seen = this.#seenSettings.get(d.id) ?? new Map<string, string>();
        for (const s of entries) seen.set(s.key, hex(toBinary(SettingSchema, s)));
        this.#seenSettings.set(d.id, seen);
        return this.#reply({ result: { case: "settings", value: { device: d.id, settings: entries, end } } });
      }
      case "setSettings":
        return this.#setSettings(command.value);
      case "listProfiles":
      case "getProfile":
      case "createProfile":
      case "copyProfile":
      case "deleteProfile":
      case "listProfileRules":
      case "setProfileRules":
        this.#needProfiles();
        return this.#runProfile(command);
      case "listFeatures":
        this.find(command.value.device);
        return this.#reply({ result: { case: "features", value: { features: [], end: true } } });
      case "listFiles":
        return this.#reply({ result: { case: "files", value: { entries: [], end: true } } });
      default:
        throw new Refusal(ErrorCode.UNKNOWN_COMMAND);
    }
  }

  #setAdapter({ name, platform, transports, configurationInterfaces }: Command<"setAdapter">) {
    const next = name === undefined ? this.name : name === "" ? this.defaultName : adapterName(name);
    if (next === null) throw new Refusal(ErrorCode.BAD_ARGS);
    if (platform !== undefined && !(platform in Platform)) throw new Refusal(ErrorCode.BAD_ARGS);
    if (transports.some((u) => u.transport === Transport.UNSPECIFIED)) throw new Refusal(ErrorCode.BAD_ARGS);
    const updates = transports.map((u) => ({ transport: this.transports.find((t) => TRANSPORT[t] === u.transport), enabled: u.enabled }));
    if (updates.some((u) => !u.transport)) throw new Refusal(ErrorCode.UNSUPPORTED);
    // Interface updates apply in order to a copy, which must be a valid configuration as a whole.
    const interfaces = structuredClone(this.interfaces);
    for (const u of configurationInterfaces) {
      if (u.interface === ConfigurationInterface.UNSPECIFIED) throw new Refusal(ErrorCode.BAD_ARGS);
      const i = (Object.keys(INTERFACE) as InterfaceName[]).find((x) => INTERFACE[x] === u.interface);
      if (!i || !this.profileSupport) throw new Refusal(ErrorCode.UNSUPPORTED);
      if (u.profile !== undefined && u.profile !== 0 && !this.profiles.some((p) => p.id === u.profile)) throw new Refusal(ErrorCode.NOT_FOUND);
      if (u.enabled !== undefined) interfaces[i].enabled = u.enabled;
      if (u.profile !== undefined) interfaces[i].profile = u.profile;
    }
    if (Object.values(interfaces).some((i) => i.enabled && !i.profile)) throw new Refusal(ErrorCode.BAD_ARGS);
    if (interfaces.via.enabled && interfaces.vial.enabled) throw new Refusal(ErrorCode.UNSUPPORTED);
    this.#needReady();
    const toggled: ("classic" | "ble")[] = [];
    for (const { transport, enabled } of updates) {
      if (enabled === undefined || this.enabled[transport!] === enabled) continue;
      this.enabled[transport!] = enabled;
      if (!toggled.includes(transport!)) toggled.push(transport!);
    }
    const reconnect = (Object.keys(interfaces) as InterfaceName[]).some((i) => {
      const [was, now] = [this.interfaces[i], interfaces[i]];
      return was.enabled !== now.enabled || (now.enabled && was.profile !== now.profile);
    });
    const interfacesChanged = JSON.stringify(interfaces) !== JSON.stringify(this.interfaces);
    const changed = next !== this.name || (platform !== undefined && Platform[platform]!.toLowerCase() !== this.platform) || toggled.length > 0 || interfacesChanged;
    this.interfaces = interfaces;
    this.name = next;
    if (platform !== undefined) this.platform = Platform[platform]!.toLowerCase() as FakeAdapter["platform"];
    this.#reply({});
    if (changed) this.#event({ case: "adapter", value: this.status() });
    for (const t of toggled) this.#applyTransport(t);
    if (reconnect) this.#reconnectUsb();
  }

  #setDevice(update: Command<"setDevice">) {
    const d = this.find(update.device);
    const { enabled, trusted, blocked, integrations } = update;
    const kinds = integrations.map((i) => i.kind);
    if (kinds.some((k) => k !== IntegrationKind.HIDPP)) throw new Refusal(ErrorCode.UNSUPPORTED);
    const layers = update.profiles?.profiles;
    if (layers) {
      if (!this.profileSupport) throw new Refusal(ErrorCode.UNSUPPORTED);
      if (layers.length > this.maxLayers) throw new Refusal(ErrorCode.BAD_ARGS);
      if (layers.some((id) => !this.profiles.some((p) => p.id === id))) throw new Refusal(ErrorCode.NOT_FOUND);
    }
    this.#needReady();
    // An update to what the adapter already holds succeeds without checking anything more.
    const same = (enabled === undefined || enabled === d.enabled) && (trusted === undefined || trusted === d.trusted)
      && (blocked === undefined || blocked === d.blocked) && integrations.every((i) => i.enabled === undefined || i.enabled === d.hidppEnabled)
      && (!layers || (layers.length === d.profiles.length && layers.every((id, i) => id === d.profiles[i])));
    if (same) return this.#reply({});
    // As the firmware does, a change that leaves an unused device enabled and not blocked needs a place.
    const unused = [InactiveReason.DISABLED, InactiveReason.BLOCKED, InactiveReason.CAPACITY].includes(this.inactive(d)!);
    if (unused && (enabled ?? d.enabled) && !(blocked ?? d.blocked)) {
      const used = this.devices.filter((x) => x !== d && x.transport === d.transport && this.inactive(x) === undefined).length;
      if (used >= this.maxEnabled) throw new Refusal(ErrorCode.NO_CAPACITY, CapacityReason.ENABLED);
    }
    const patch: Partial<FakeDevice> = {};
    if (enabled !== undefined) patch.enabled = enabled;
    if (trusted !== undefined) patch.trusted = trusted;
    if (blocked !== undefined) patch.blocked = blocked;
    if (layers) patch.profiles = [...layers];
    // Integration updates apply in order; the last one for HID++ wins.
    for (const i of integrations) if (i.enabled !== undefined) patch.hidppEnabled = i.enabled;
    if ((patch.blocked || patch.enabled === false) && d.state !== "disconnected") patch.state = "disconnected";
    const before = this.#snapshot(d);
    const loads = this.#loads();
    Object.assign(d, patch);
    // Loading follows the new layers before the response, as the firmware's does.
    this.#load(null);
    this.#reply({});
    this.#changed(d, before, loads);
  }

  #setSettings({ device: id, changes }: Command<"setSettings">) {
    const d = this.find(id);
    if (!changes.length) throw new Refusal(ErrorCode.BAD_ARGS);
    // Every change is checked before any is saved.
    const checked = changes.map((c) => {
      const s = d.settings.find((x) => x.key === c.key);
      if (!s || c.integration !== IntegrationKind.HIDPP) throw new Refusal(ErrorCode.NOT_FOUND);
      if (c.change.case === "forget") return { s, value: null };
      const value = fromWire(s, c.change.value);
      if (value === undefined) throw new Refusal(ErrorCode.BAD_ARGS);
      return { s, value };
    });
    this.#needReady();
    // In order, so a later change to the same setting replaces an earlier one.
    for (const { s, value } of checked) Object.assign(s, { saved: value, state: "pending", error: null });
    this.#reply({});
    this.#settingsChanged(d);
    this.#apply(d);
  }

  // ---- Profiles -------------------------------------------------------------

  #profile(id: number): FakeProfile {
    const p = this.profiles.find((x) => x.id === id);
    if (!p) throw new Refusal(ErrorCode.NOT_FOUND);
    return p;
  }

  #runProfile(command: Extract<Request["command"], { case: "listProfiles" | "getProfile" | "createProfile" | "copyProfile" | "deleteProfile" | "listProfileRules" | "setProfileRules" }>) {
    switch (command.case) {
      case "listProfiles": {
        const { entries, end } = page(this.profiles, command.value.after, this.pageSize, (p) => p.id, (a, b) => a - b);
        const listed = entries.map((p) =>
          this.newerProfiles.has(p.id) ? { entry: { case: undefined } }
            : this.unreadableProfiles.has(p.id) ? { entry: { case: "unreadable" as const, value: p.id } } : { entry: { case: "profile" as const, value: this.#profileRecord(p) } });
        return this.#reply({ result: { case: "profiles", value: { entries: listed, end } } });
      }
      case "getProfile": {
        const p = this.#profile(command.value.profile);
        if (this.unreadableProfiles.has(p.id)) throw new Refusal(ErrorCode.STORAGE_FAILED);
        return this.#reply({ result: { case: "profile", value: this.#profileRecord(p) } });
      }
      case "createProfile":
      case "copyProfile": {
        const source = command.case === "copyProfile" ? this.#profile(command.value.profile) : null;
        const { name } = command.value;
        if (adapterName(name) === null) throw new Refusal(ErrorCode.BAD_ARGS);
        this.#needReady();
        if (this.storageFull) throw new Refusal(ErrorCode.NO_CAPACITY, CapacityReason.STORAGE);
        const created: FakeProfile = { id: this.#nextProfile++, name, rules: source ? source.rules.map((r) => create(ProfileRuleSchema, r)) : [] };
        if (source?.size !== undefined) created.size = source.size;
        this.profiles.push(created);
        this.#reply({ result: { case: "profileCreated", value: { profile: created.id } } });
        this.#event({ case: "profile", value: this.#profileRecord(created) });
        return;
      }
      case "deleteProfile": {
        const p = this.#profile(command.value.profile);
        const used = Object.values(this.interfaces).some((i) => i.profile === p.id) || this.devices.some((d) => d.profiles.includes(p.id));
        if (used) throw new Refusal(ErrorCode.IN_USE);
        this.#needReady();
        this.profiles = this.profiles.filter((x) => x !== p);
        this.#reply({});
        this.#event({ case: "profileRemoved", value: { id: p.id } });
        return;
      }
      case "listProfileRules": {
        const p = this.#profile(command.value.profile);
        const { entries, end } = page(p.rules, command.value.after, this.pageSize, ruleInput, compareUsages);
        return this.#reply({ result: { case: "profileRules", value: { profile: p.id, rules: entries, end } } });
      }
      case "setProfileRules": {
        const p = this.#profile(command.value.profile);
        if (!command.value.changes.length) throw new Refusal(ErrorCode.BAD_ARGS);
        const rules = [...p.rules];
        for (const change of command.value.changes) {
          const c = change.change;
          if (c.case === undefined) throw new Refusal(ErrorCode.BAD_ARGS);
          const ref = c.value;
          if (!ref.input) throw new Refusal(ErrorCode.BAD_ARGS);
          const i = rules.findIndex((r) => sameUsage(r.input, ref.input));
          if (i !== -1) rules.splice(i, 1);
          if (c.case === "forget") continue;
          const effect = c.value.effect;
          if (effect.case === undefined) throw new Refusal(ErrorCode.BAD_ARGS);
          if (effect.case === "remap" && effect.value.outputs.length > 8) throw new Refusal(ErrorCode.BAD_ARGS);
          if (effect.case === "scale" && (effect.value.numerator === 0 || effect.value.denominator === 0)) throw new Refusal(ErrorCode.BAD_ARGS);
          // A rule that changes nothing is forgotten instead.
          const identity = effect.case === "remap"
            ? effect.value.outputs.length === 1 && sameUsage(effect.value.outputs[0]!.usage, c.value.input)
            : effect.value.numerator === effect.value.denominator;
          if (!identity) rules.push(create(ProfileRuleSchema, c.value));
        }
        const grown = { ...p, rules };
        const loaded = [...this.#loaded.values()].some((ids) => ids.includes(p.id));
        const after = loaded ? this.memoryUsed() - sizeOf(p) + sizeOf(grown) : sizeOf(grown);
        if (sizeOf(grown) > this.memoryBudget || after > this.memoryBudget) throw new Refusal(ErrorCode.NO_CAPACITY, CapacityReason.PROFILE_MEMORY);
        this.#needReady();
        const roles = rolesOf(p).join();
        const loads = this.#loads();
        const before = new Map(p.rules.map((r) => [`${r.input?.usagePage}:${r.input?.usage}`, encodedRule(r)]));
        const keyOf = (r: ProfileRule) => `${r.input?.usagePage}:${r.input?.usage}`;
        p.rules = rules.sort((a, b) => compareUsages(ruleInput(a), ruleInput(b)));
        const changed = p.rules.filter((r) => before.get(keyOf(r)) !== encodedRule(r));
        const removed = [...before.keys()].filter((k) => !p.rules.some((r) => keyOf(r) === k)).map((k) => {
          const [usagePage, usage] = k.split(":").map(Number);
          return { usagePage: usagePage!, usage: usage! };
        });
        this.#reply({});
        if (changed.length || removed.length) this.#event({ case: "profileRulesChanged", value: { profile: p.id, changed, removed } });
        if (rolesOf(p).join() !== roles) this.#event({ case: "profile", value: this.#profileRecord(p) });
        this.#reloadUsers(p.id, loads);
        return;
      }
    }
  }

  /** Applies a transport change as the firmware does after saving it: disabling one closes its
   * links, ends a pairing over it as an unsupported transport would and drops it from a running
   * scan, ending the scan when no transport is left. Its saved devices report their new state. */
  #applyTransport(transport: "classic" | "ble") {
    const on = this.enabled[transport];
    if (!on) {
      const p = this.#pairing;
      if (p && (p.candidate.transport ?? "ble") === transport) {
        this.#pairing = null;
        this.#pairingEvent({ case: "failed", value: ErrorCode.UNSUPPORTED }, p.candidate.id);
      }
      if (this.#scan) {
        this.#scanning = this.#scanning.filter((t) => t !== transport);
        if (!this.#scanning.length) this.#stopScan(this.candidates.length);
      }
    }
    for (const d of this.devices.filter((x) => x.transport === transport)) {
      const before = this.#snapshot(d);
      if (!on) d.state = "disconnected";
      this.#changed(d, before);
    }
  }

  #stopScan(count: number) {
    if (!this.#scan) return;
    for (const t of this.#scan) clearTimeout(t);
    this.#scan = null;
    this.#event({ case: "scanDone", value: { count, truncated: false } });
  }

  #pairingEvent(step: MessageInitShape<typeof PairingSchema>["step"], candidate = this.#pairing?.candidate.id ?? 0) {
    this.#event({ case: "pairing", value: { candidate, step } });
  }

  #paired() {
    const p = this.#pairing;
    if (!p) return;
    this.#pairing = null;
    const created = device(this.#nextDevice++, {
      name: p.candidate.name,
      kinds: p.candidate.kinds,
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
