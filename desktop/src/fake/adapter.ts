// A simulated adapter speaking the serial API over an in-memory ByteStream,
// following the firmware's behavior: every command answers at once, and
// changes follow as events after the response. Each listing is one response
// frame holding Messages of `batchSize` entries encoded back to back, as the
// firmware streams them, and settings events carry only what changed since the
// client last listed or heard of them. Tests and the demo
// mode use it; all data is synthetic.
import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import { compareRuleRefs, compareSettingsRefs, ruleRef, settingsRef, type ByteStream } from "@cordial/client";
import {
  CodeKind,
  ConfigurationInterface,
  DELIMITER,
  DeviceSchema,
  DeviceState,
  ErrorCode,
  ErrorOutcome,
  ErrorReason,
  FrameDecoder,
  InactiveReason,
  IntegrationKind,
  IntegrationState,
  Kind,
  MessageSchema,
  OutputMode,
  OutputState,
  Platform,
  ProfileRuleSchema,
  RequestSchema,
  ResourceType,
  Role,
  SettingState,
  SettingsGroup,
  SettingsObjectSchema,
  StatusSchema,
  Transport,
  encodeFrame,
  type DeviceInformationSchema,
  type ErrorSchema,
  type EventSchema,
  type PairingSchema,
  type ProfileRule,
  type Request,
  type ResponseSchema,
  type SettingsObject,
  type Usage,
  UsageSchema,
  UsageRange_Kind,
  VendorRegistry,
} from "@cordial/protocol";
import { CASES, FIELDS, updateChanges } from "../core/convert.ts";
import { adapterName } from "../shared/adapter-name.ts";
import type { Candidate, KindName, OutputModeName, RoleName, Scalar, SettingField, SettingFields, SettingsGroupName, ValueType } from "../shared/state.ts";

type MessageInit = MessageInitShape<typeof MessageSchema>;
export type ErrorInit = MessageInitShape<typeof ErrorSchema>;
type Command<C extends Request["command"]["case"]> = Extract<Request["command"], { case: C }>["value"];

/** A HID++ setting field: its group, component index (pointer sensors only) and field. Enum
 * values are the protocol's names in lower case. */
export type FakeSetting = Omit<SettingField, "integration"> & {
  type: ValueType;
  value: Scalar | null;
  saved: Scalar | null;
  /** `unsupported` reports the saved setting as unsupported. */
  state: "unknown" | "pending" | "applied" | "changed_on_device" | "unsupported";
  /** The last apply's failure, replacing the state. */
  error: ErrorCode | ErrorInit | null;
  choices: Scalar[];
  min: number | null;
  max: number | null;
  step: number | null;
  /** Applying the saved value fails with this, though it stays saved. */
  fails: ErrorCode | ErrorInit | null;
};

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
  /** The last failed connection attempt. */
  error: ErrorCode | ErrorInit | null;
  hidppEnabled: boolean;
  /** The HID++ version the device reports when connected; null for a device without HID++. */
  hidpp: [number, number] | null;
  /** Replaces the HID++ state the fake derives while connected, such as `STARTING`. */
  hidppState: IntegrationState | null;
  /** HID++ failed to start on the connected device. */
  hidppError: ErrorCode | ErrorInit | null;
  roles: RoleName[];
  /** What the device reports while connected. */
  info: MessageInitShape<typeof DeviceInformationSchema>;
  /** Settings as read on the current connection, with the saved values. */
  settings: FakeSetting[];
  /** The device's layers: profile IDs in the order they apply. */
  profiles: number[];
  /** Why the connected device's profiles aren't loaded; the fake works it out. */
  profileError: ErrorCode | null;
  /** The saved Gamepad Output mode. */
  gamepadOutput: OutputModeName;
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
  /** Entries per Message in a listing's response frame. */
  batchSize?: number;
  /** USB interfaces the board can expose. */
  usbInterfaces?: number;
  /** Whether the board has HID and XInput gamepad outputs; every board but the ESP32-S3 does. */
  gamepadOutputs?: boolean;
}

/** The memory in use and each device's profile error, before a change that may load profiles. */
interface Loads {
  used: number;
  errors: Map<number, ErrorCode | null>;
}

/** Bytes a loaded profile takes, and each of its rules. */
const PROFILE_BYTES = 64;
const RULE_BYTES = 16;

type Pair = [number, number];

const usage = (usagePage: number, u: number) => ({ usagePage, usage: u });

const KEYBOARD = usage(0x01, 0x06);
const MOUSE = usage(0x01, 0x02);
const CONSUMER = usage(0x0c, 0x01);
const SYSTEM = usage(0x01, 0x80);
const GAME_PAD = usage(0x01, 0x05);

const REMAP_INPUTS = [
  { collection: KEYBOARD, usagePage: 0x07, min: 0x04, max: 0xa4 },
  { collection: KEYBOARD, usagePage: 0x07, min: 0xe0, max: 0xe7 },
  { collection: MOUSE, usagePage: 0x09, min: 0x01, max: 0x10 },
  { collection: CONSUMER, usagePage: 0x0c, min: 0x01, max: 0x29c },
  { collection: SYSTEM, usagePage: 0x01, min: 0x81, max: 0xb7 },
  { collection: GAME_PAD, usagePage: 0x09, min: 0x01, max: 0x20 },
  { collection: GAME_PAD, usagePage: 0x01, min: 0x3d, max: 0x3e },
  { collection: GAME_PAD, usagePage: 0x01, min: 0x90, max: 0x93 },
].map((r) => ({ ...r, kind: UsageRange_Kind.REMAP_INPUT }));
const SCALE_INPUTS = [
  { collection: MOUSE, usagePage: 0x01, min: 0x30, max: 0x31 },
  { collection: MOUSE, usagePage: 0x01, min: 0x38, max: 0x38 },
  { collection: MOUSE, usagePage: 0x0c, min: 0x238, max: 0x238 },
].map((r) => ({ ...r, kind: UsageRange_Kind.SCALE_INPUT }));
const REMAP_OUTPUTS = [
  { collection: KEYBOARD, usagePage: 0x07, min: 0x04, max: 0xe7 },
  { collection: MOUSE, usagePage: 0x09, min: 0x01, max: 0x05 },
  { collection: CONSUMER, usagePage: 0x0c, min: 0x01, max: 0x29c },
].map((r) => ({ ...r, kind: UsageRange_Kind.REMAP_OUTPUT }));
/** Game Pad buttons, Start, Select and D-pad, which only a board with gamepad outputs sends. */
const GAME_PAD_OUTPUTS = REMAP_INPUTS.filter((r) => r.collection === GAME_PAD).map((r) => ({ ...r, kind: UsageRange_Kind.REMAP_OUTPUT }));
/** Game Pad X, Y, Z, Rx, Ry and Rz, and the Simulation page's Brake and Accelerator. */
const GAME_PAD_AXES = [
  { collection: GAME_PAD, usagePage: 0x01, min: 0x30, max: 0x35 },
  { collection: GAME_PAD, usagePage: 0x02, min: 0xc4, max: 0xc5 },
];

type Range = (typeof REMAP_INPUTS)[number];
type UsageKey = Pick<Usage, "usagePage" | "usage">;
const sameUsage = (a: UsageKey | undefined, b: UsageKey | undefined) => a?.usagePage === b?.usagePage && a?.usage === b?.usage;
/** Whether one of `ranges` grants `kind` for `u` in `collection`. */
const within = (ranges: Range[], kind: UsageRange_Kind, u: UsageKey | undefined, collection: UsageKey | undefined) =>
  ranges.some((r) => r.kind === kind && sameUsage(r.collection, collection) && r.usagePage === u?.usagePage && u.usage >= r.min && u.usage <= r.max);

/** A remap rule from one HID usage in `collection` to others, in `outputCollection` when given. */
export function remap(input: Pair, collection: Pair, outputs: Pair[], outputCollection?: Pair): ProfileRule {
  return create(ProfileRuleSchema, {
    input: usage(...input),
    collection: usage(...collection),
    effect: {
      case: "remap",
      value: { outputs: outputs.map((o) => ({ usage: usage(...o), ...(outputCollection ? { collection: usage(...outputCollection) } : {}) })) },
    },
  });
}

/** A scale rule on a relative HID usage in `collection`. */
export function scale(input: Pair, collection: Pair, numerator: number, denominator = 1): ProfileRule {
  return create(ProfileRuleSchema, { input: usage(...input), collection: usage(...collection), effect: { case: "scale", value: { numerator, denominator } } });
}

/** An axis rule routing a Game Pad axis to another. */
export function axis(input: Pair, destination: Pair, invert = false): ProfileRule {
  return create(ProfileRuleSchema, { input: usage(...input), collection: GAME_PAD, effect: { case: "axis", value: { destination: usage(...destination), invert } } });
}

/** One rule giving each role, for tests and demos. */
const ROLE_RULES: Record<RoleName, ProfileRule> = {
  keyboard: remap([0x07, 0x39], [0x01, 0x06], [[0x07, 0x29]]),
  mouse: scale([0x01, 0x38], [0x01, 0x02], -1),
  consumer_control: remap([0x0c, 0xe9], [0x0c, 0x01], [[0x0c, 0xea]]),
  system_control: remap([0x01, 0x82], [0x01, 0x80], []),
  gamepad: axis([0x01, 0x30], [0x01, 0x31]),
};

/** A profile whose rules give it `roles`, for tests and demos. */
export function profile(id: number, name: string, roles: RoleName[] = [], size?: number): FakeProfile {
  return { id, name, rules: roles.map((r) => ROLE_RULES[r]), ...(size === undefined ? {} : { size }) };
}

/** The role one rule adds, from its input's collection. */
const ruleRole = (rule: ProfileRule): RoleName | null =>
  ([[KEYBOARD, "keyboard"], [MOUSE, "mouse"], [CONSUMER, "consumer_control"], [SYSTEM, "system_control"], [GAME_PAD, "gamepad"]] as const)
    .find(([c]) => sameUsage(c, rule.collection))?.[1] ?? null;

const ROLE_ORDER: RoleName[] = ["keyboard", "mouse", "consumer_control", "system_control", "gamepad"];
const rolesOf = (p: FakeProfile) => ROLE_ORDER.filter((r) => p.rules.some((rule) => ruleRole(rule) === r));
const sizeOf = (p: FakeProfile) => p.size ?? PROFILE_BYTES + RULE_BYTES * p.rules.length;
const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

/** `list` sorted by `compare` on each entry's key. */
const sorted = <E, K>(list: E[], key: (e: E) => K, compare: (a: K, b: K) => number) => [...list].sort((a, b) => compare(key(a), key(b)));

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
    profiles: [],
    profileError: null,
    gamepadOutput: "disabled",
    ...patch,
  };
}

export function setting<G extends SettingsGroupName>(group: G, field: SettingFields[G], patch: Partial<FakeSetting> = {}): FakeSetting {
  const type = (FIELDS[group] as Record<string, { type: ValueType }>)[field]!.type;
  return { group, field, index: group === "pointer_sensor" ? 0 : null, type, value: null, saved: null, state: "pending", error: null, choices: [], min: null, max: null, step: null, fails: null, ...patch } as FakeSetting;
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
        identity: {
          manufacturer: "Example Co",
          model: "Example Keys",
          serial: "0000EXAMPLE1",
          firmwareVersion: "EK 1.02.0003",
          bootloaderVersion: "BL 1.00.0001",
          vendor: { registry: VendorRegistry.USB, id: 0x1234 },
          product: { id: 0x5678, version: 1 },
        },
        battery: { level: 72, charging: false },
        backlight: { currentBrightness: 3 },
      },
      settings: [
        setting("keyboard", "function_row", { choices: ["function_keys", "special_actions"], value: "special_actions" }),
        setting("keyboard", "platform", { choices: ["windows", "linux", "chrome_os", "android", "mac", "ios"], value: "linux" }),
        setting("backlight", "enabled", { value: true, saved: true, state: "applied" }),
        setting("backlight", "mode", { choices: ["automatic", "permanent_manual"], value: "automatic" }),
        setting("backlight", "brightness", { min: 0, max: 7, step: 1, value: 3 }),
        setting("backlight", "hands_away", { min: 5, max: 7200, step: 5, value: 60, saved: 30, state: "changed_on_device" }),
        setting("power", "auto_off", { min: 0, max: 15300, step: 60, value: 1800 }),
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
        identity: { manufacturer: "Example Co" },
        battery: { level: 12, charging: false },
        wheel: { resolutionMultiplier: 8, ratchetsPerRotation: 24, diameter: 50 },
      },
      settings: [
        setting("pointer_sensor", "dpi", { min: 200, max: 8000, step: 50, value: 1000 }),
        setting("wheel", "mode", { choices: ["free_spin", "stepped"], value: "stepped" }),
        setting("wheel", "invert", { value: false }),
      ],
    }),
    device(3, { name: "Travel Keyboard", transport: "classic", info: { identity: { firmwareVersion: "2.1" } } }),
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
      info: { identity: { manufacturer: "Example Co" }, battery: { level: 54, charging: true } },
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

const BOARDS: Record<string, string> = { pico_w: "Pico W", pico2_w: "Pico 2 W", esp32s3: "ESP32-S3", waveshare_rp2350b_plus_w: "RP2350B-Plus-W" };
const TRANSPORT = { classic: Transport.CLASSIC, ble: Transport.BLE } as const;
const INTERFACE = { via: ConfigurationInterface.VIA, vial: ConfigurationInterface.VIAL } as const;
const OUTPUT = { disabled: OutputMode.DISABLED, hid: OutputMode.HID, xinput: OutputMode.XINPUT } as const;
/** Slots in each gamepad mode's pool at most. */
const MAX_SLOTS = 4;
/** The marker Vial looks for after the adapter ID in the USB serial number. */
const VIAL_SERIAL = "-vial:f64c2b3c";
const upper = <E>(e: E, name: string) => (e as Record<string, number>)[name.toUpperCase()]!;

/** The protocol value of a fake setting's value, by its field's type. */
function wireOf(s: FakeSetting, v: Scalar): boolean | bigint | number {
  if (s.type === "integer") return BigInt(v as number);
  if (s.type === "bool") return v as boolean;
  return upper((FIELDS[s.group] as Record<string, { enum?: object }>)[s.field]!.enum, v as string);
}

/** Whether the setting takes `v`, a value read from the protocol. */
function takes(s: FakeSetting, v: Scalar): boolean {
  if (s.type === "bool") return typeof v === "boolean";
  if (s.choices.length || s.type === "enum") return s.choices.includes(v);
  const n = v as number;
  return (s.min === null || n >= s.min) && (s.max === null || n <= s.max) && (s.step === null || (n - (s.min ?? 0)) % s.step === 0);
}

const sameObject = (s: FakeSetting, group: string, index: number | null) => s.group === group && s.index === index;

/** A saved record's failed read. */
const READ_ERROR: ErrorInit = { code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.NO_EFFECT };

/** The reason the firmware gives a code that comes without one here. */
const REASONS: Partial<Record<ErrorCode, ErrorReason>> = {
  [ErrorCode.NOT_READY]: ErrorReason.RADIO,
  [ErrorCode.NO_CAPACITY]: ErrorReason.ENABLED_DEVICES,
  [ErrorCode.STORAGE_FAILED]: ErrorReason.STORAGE,
  [ErrorCode.STORAGE_FULL]: ErrorReason.STORAGE,
  [ErrorCode.PROFILE_MEMORY_LOW]: ErrorReason.PROFILE_MEMORY,
  [ErrorCode.USB_REQUIRED_CAPACITY_EXCEEDED]: ErrorReason.OUTPUT_BACKEND,
  [ErrorCode.USB_RESERVED_CAPACITY_EXCEEDED]: ErrorReason.OUTPUT_BACKEND,
  [ErrorCode.PROFILE_MEMORY_EXCEEDED]: ErrorReason.PROFILE_MEMORY,
  [ErrorCode.PROFILE_LOAD_FAILED]: ErrorReason.STORAGE,
};

/** An error as the protocol carries it, from a code alone, with the firmware's reason, or a
 * complete error. */
const errorOf = (e: ErrorCode | ErrorInit): ErrorInit => (typeof e === "number" ? { code: e, reason: REASONS[e] } : e);

/** The firmware's order of errors: by numeric code, reason and outcome, then related errors in
 * turn, a shorter list first. */
function compareErrors(a: ErrorInit, b: ErrorInit): number {
  const order = (a.code ?? 0) - (b.code ?? 0) || (a.reason ?? 0) - (b.reason ?? 0) || (a.outcome ?? 0) - (b.outcome ?? 0);
  if (order) return order;
  const x = a.related ?? [];
  const y = b.related ?? [];
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    const related = compareErrors(x[i]!, y[i]!);
    if (related) return related;
  }
  return x.length - y.length;
}

/** Errors in the firmware's order without duplicates. */
const errorList = (list: ErrorInit[]) => list.sort(compareErrors).filter((e, i, all) => !i || compareErrors(all[i - 1]!, e));

class Refusal extends Error {
  readonly error: ErrorInit;
  constructor(code: ErrorCode | ErrorInit, reason?: ErrorReason) {
    const error = typeof code === "number" ? { ...errorOf(code), ...(reason === undefined ? {} : { reason }) } : code;
    super(ErrorCode[error.code ?? ErrorCode.UNKNOWN]);
    this.error = error;
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
  /** Storage in use and the filesystem's size, in bytes; null while unknown. */
  storageUsed: number | null = 24576;
  storageTotal: number | null = 524288;
  readonly usbInterfaces: number;
  maxEnabled: number;
  /** Requests received in this session, for assertions. */
  received: Request[] = [];
  /** Events sent in this session, for assertions. */
  events: NonNullable<MessageInitShape<typeof EventSchema>["kind"]>[] = [];
  /** Errors to answer the next requests of a command with, in order, by command case. A code alone
   * gets the reason the firmware would give it. */
  failures: Record<string, (ErrorCode | ErrorInit)[]> = {};
  /** Errors that end the next listings of a command after their first Message of entries, in
   * order, by command case. */
  lateFailures: Record<string, (ErrorCode | ErrorInit)[]> = {};
  /** Whether a STORAGE_FAILED failure leaves the outcome of its write unknown. */
  uncertainWrites = false;
  /** The error the next scan ends with, right after its last candidate, keeping its candidates. */
  scanError: ErrorInit | null = null;
  /** Radio, storage and other faults the status reports along with what the state shows. */
  faults: ErrorInit[] = [];
  /** The error the next pairing ends with in place of authenticating. */
  pairingError: ErrorInit | null = null;
  readonly profileSupport: boolean;
  readonly gamepadOutputs: boolean;
  profiles: FakeProfile[];
  /** Saved configuration interface preferences, in ConfigurationInterface order. */
  interfaces: Record<InterfaceName, { enabled: boolean; profile: number }>;
  memoryBudget: number;
  maxLayers: number;
  batchSize: number;
  /** Records whose flash read fails: listed with a read error, and a profile here can't load. */
  unreadableDevices = new Set<number>();
  unreadableProfiles = new Set<number>();
  /** Records listed as entries of a kind newer than the client, which it reads as unset. */
  newerDevices = new Set<number>();
  newerProfiles = new Set<number>();
  /** How often USB reconnected for a change to its interfaces. */
  usbReconnects = 0;
  /** Whether the USB device is attached; false briefly while it reconnects. */
  present = true;
  /** Bytes from an earlier session delivered right after the next open. */
  staleInput: Uint8Array | null = null;
  /** Called when the USB device is attached again after reconnecting. */
  readonly #hotplug: (() => void)[] = [];
  /** Profiles loaded for each connected device, by device ID. */
  readonly #loaded = new Map<number, number[]>();
  /** The gamepad slot each connected device holds in its mode's pool, by device ID. */
  readonly #slots = new Map<number, { mode: OutputModeName; slot: number }>();
  /** The USB interfaces exposed since USB last connected, and the priorities of those left out. */
  #usb: { exposed: string[]; omitted: number[] };
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
  /** The settings objects of each device this session's client has listed or heard of, by their
   * identity, with their encoding, so events carry only what changed since. */
  readonly #seenSettings = new Map<number, Map<string, string>>();

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
    this.gamepadOutputs = options.gamepadOutputs ?? this.board !== "esp32s3";
    this.profiles = this.profileSupport ? (options.profiles ?? []) : [];
    if (!this.profileSupport) for (const d of this.devices) d.profiles = [];
    this.interfaces = {
      via: { enabled: false, profile: 0, ...options.interfaces?.via },
      vial: { enabled: false, profile: 0, ...options.interfaces?.vial },
    };
    this.memoryBudget = options.memoryBudget ?? 4096;
    this.maxLayers = options.maxLayers ?? 4;
    this.batchSize = options.batchSize ?? 8;
    this.usbInterfaces = options.usbInterfaces ?? 16;
    this.#nextProfile = Math.max(0, ...this.profiles.map((p) => p.id)) + 1;
    this.#nextDevice = Math.max(9, ...this.devices.map((d) => d.id)) + 1;
    this.#load(null);
    this.#usb = this.#allocate();
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

  /** Queues messages, encoded back to back in one frame, behind earlier ones, after the reply
   * delay. */
  #message(...messages: MessageInit[]) {
    const session = this.#session;
    const frame = encodeFrame(new Uint8Array(messages.flatMap((m) => [...toBinary(MessageSchema, create(MessageSchema, m))])));
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

  /** Answers a listing with every entry in one frame, `batchSize` entries per Message. A late
   * failure for `command` follows the first Message. */
  #list<E>(command: string, entries: E[], result: (batch: E[]) => MessageInitShape<typeof ResponseSchema>["result"]) {
    const late = this.lateFailures[command]?.shift();
    const pieces: MessageInit[] = [];
    for (let i = 0; i === 0 || i < entries.length; i += this.batchSize) {
      pieces.push({ kind: { case: "response", value: { result: result(entries.slice(i, i + this.batchSize)) } } });
      if (late !== undefined) {
        pieces.push({ kind: { case: "response", value: { result: { case: "error", value: errorOf(late) } } } });
        break;
      }
    }
    this.#message(...pieces);
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

  /** The adapter's problems, in the firmware's order: `faults`, then what the adapter's state
   * shows. */
  errors(): ErrorInit[] {
    const omitted = this.#usb.omitted;
    const codes = [
      ...(omitted.some((p) => p < 0xc0) ? [ErrorCode.USB_REQUIRED_CAPACITY_EXCEEDED] : []),
      ...(omitted.some((p) => p >= 0xc0 && p < 0xe0) ? [ErrorCode.USB_RESERVED_CAPACITY_EXCEEDED] : []),
      ...(this.ready && this.storageFull ? [ErrorCode.STORAGE_FULL] : []),
      ...(this.profileSupport && this.memoryBudget > 0 && this.memoryUsed() * 100 >= this.memoryBudget * 85 ? [ErrorCode.PROFILE_MEMORY_LOW] : []),
    ];
    return errorList([...this.faults, ...codes.map(errorOf)]);
  }

  /** The USB interfaces the firmware would expose, with their priorities, lower first: serial
   * (two), keyboard and mouse while a saved device uses them, the slots of each gamepad mode up to
   * its maximum, and each enabled configuration interface. */
  #wanted(): { name: string; priority: number }[] {
    const live = (d: FakeDevice) => d.state === "connected" && this.inactive(d) === undefined;
    const desktop = this.devices.filter((d) => d.roles.some((r) => r !== "gamepad"));
    const want = [{ name: "serial", priority: 0x00 }, { name: "serial", priority: 0x00 }];
    if (desktop.length) want.push({ name: "keyboard", priority: desktop.some(live) ? 0x41 : 0x81 });
    if (this.gamepadOutputs)
      for (const [mode, offset] of [["hid", 2], ["xinput", 3]] as const) {
        const users = this.devices.filter((d) => d.gamepadOutput === mode);
        const connected = users.filter(live).length;
        for (let slot = 0; slot < MAX_SLOTS; slot++)
          want.push({ name: mode, priority: offset + (slot < connected ? 0x40 : slot >= users.length ? 0xe0 : slot === 0 ? 0x80 : 0xc0) });
      }
    for (const i of ["via", "vial"] as const) {
      const { enabled, profile } = this.interfaces[i];
      if (enabled) want.push({ name: i, priority: this.devices.some((d) => live(d) && d.profiles.includes(profile)) ? 0x44 : 0xc4 });
    }
    return want;
  }

  /** What fits the board's interfaces, lowest priority first. */
  #allocate() {
    const want = this.#wanted().sort((a, b) => a.priority - b.priority);
    return { exposed: want.slice(0, this.usbInterfaces).map((w) => w.name), omitted: want.slice(this.usbInterfaces).map((w) => w.priority) };
  }

  /** Allocates USB again after a save, reconnecting it once the response has gone out when the
   * exposed interfaces change. */
  #reallocate() {
    const before = this.#usb.exposed.join();
    this.#usb = this.#allocate();
    if (this.#usb.exposed.join() !== before) this.#reconnectUsb();
  }

  status() {
    return create(StatusSchema, {
      id: this.id,
      name: this.name,
      platform: upper(Platform, this.platform),
      ready: this.ready,
      errors: this.errors(),
      resources: [
        { type: ResourceType.INTERFACES, count: this.#usb.exposed.length, limit: this.usbInterfaces },
        { type: ResourceType.STORAGE, count: this.storageUsed ?? undefined, limit: this.storageTotal ?? undefined },
      ],
      transports: this.transports.map((t) => ({
        transport: TRANSPORT[t],
        enabled: this.enabled[t],
        resources: [
          { type: ResourceType.DEVICES, count: this.devices.filter((d) => d.transport === t && d.enabled).length, limit: this.maxEnabled },
          { type: ResourceType.CONNECTIONS, count: this.devices.filter((d) => d.transport === t && d.state === "connected").length, limit: this.maxEnabled },
        ],
      })),
      information: { firmwareVersion: "0.0.0-dev", boardName: this.board, developmentBuild: true, storageFormat: 2 },
      outputSupport: this.gamepadOutputs
        ? [{
            role: Role.GAMEPAD,
            modes: (["disabled", "hid", "xinput"] as const).map((m) => ({
              mode: OUTPUT[m],
              resources: [m === "disabled" ? { type: ResourceType.SLOTS, count: 0, limit: 0 } : { type: ResourceType.SLOTS, count: this.pool(m), limit: MAX_SLOTS }],
            })),
          }]
        : [],
      ...(this.profileSupport
        ? {
            profileSupport: {
              ranges: this.#ranges(),
              resources: [
                { type: ResourceType.MEMORY, count: this.memoryUsed(), limit: this.memoryBudget },
                { type: ResourceType.OUTPUTS, limit: 8 },
                { type: ResourceType.LAYERS, limit: this.maxLayers },
              ],
            },
            configurationInterfaces: (["via", "vial"] as const).map((i) => ({
              interface: INTERFACE[i],
              enabled: this.interfaces[i].enabled,
              active: this.#usb.exposed.includes(i),
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

  /** The size of a gamepad mode's pool: its slots in the USB allocation. */
  pool(mode: OutputModeName) {
    return this.#usb.exposed.filter((n) => n === mode).length;
  }

  /** The gamepad output's state, as the firmware works it out: a connected device with a gamepad
   * role and a supported mode keeps its slot, or takes the lowest free one in its mode's pool. */
  #gamepad(d: FakeDevice) {
    const mode = d.gamepadOutput;
    const held = this.#slots.get(d.id);
    this.#slots.delete(d.id);
    let state: OutputState | ErrorInit;
    let slot: number | undefined;
    if (mode === "disabled") state = OutputState.DISABLED;
    else if (!this.gamepadOutputs) state = { code: ErrorCode.UNSUPPORTED, reason: ErrorReason.OUTPUT_BACKEND };
    else if (d.state !== "connected" || this.inactive(d) !== undefined) state = OutputState.DISCONNECTED;
    else if (!d.roles.includes("gamepad")) state = { code: ErrorCode.UNSUPPORTED, reason: ErrorReason.INPUT_LAYOUT };
    else {
      const taken = [...this.#slots.values()].filter((s) => s.mode === mode).map((s) => s.slot);
      slot = held?.mode === mode && held.slot < this.pool(mode) ? held.slot : [...Array(this.pool(mode)).keys()].find((n) => !taken.includes(n));
      if (slot !== undefined) this.#slots.set(d.id, { mode, slot });
      state = slot === undefined ? OutputState.PENDING : OutputState.ACTIVE;
    }
    const status = typeof state === "number" ? { case: "state" as const, value: state } : { case: "error" as const, value: state };
    return { role: Role.GAMEPAD, mode: OUTPUT[mode], status, slot };
  }

  #hidpp(d: FakeDevice) {
    const connected = d.state === "connected";
    const detected = connected && d.hidpp !== null;
    if (!d.hidppEnabled && !detected) return [];
    let status: { case: "state"; value: IntegrationState } | { case: "error"; value: ErrorInit };
    if (!d.hidppEnabled) status = { case: "state", value: IntegrationState.OFF };
    else if (!connected) status = { case: "state", value: IntegrationState.DISCONNECTED };
    else if (d.hidppError !== null) status = { case: "error", value: errorOf(d.hidppError) };
    else if (d.hidppState !== null) status = { case: "state", value: d.hidppState };
    else status = d.hidpp ? { case: "state", value: IntegrationState.ACTIVE } : { case: "error", value: { code: ErrorCode.PROTOCOL_UNSUPPORTED } };
    return [{
      kind: IntegrationKind.HIDPP,
      enabled: d.hidppEnabled,
      detected: detected ? { version: { major: d.hidpp![0], minor: d.hidpp![1] } } : undefined,
      status,
    }];
  }

  record(d: FakeDevice) {
    const connected = d.state === "connected";
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
      errors: errorList([d.error, connected ? d.profileError : null].filter((e) => e !== null).map(errorOf)),
      security: connected ? { encrypted: true, authenticated: false, secureConnections: true, keySize: 16 } : undefined,
      integrations: this.#hidpp(d),
      // Information belongs to the current connection.
      information: connected ? d.info : undefined,
      roles: d.roles.map((r) => upper(Role, r)),
      profiles: this.profileSupport ? { profiles: d.profiles } : undefined,
      // The adapter keeps no entry for Disabled, and reports a gamepad's output state.
      outputPreferences: d.gamepadOutput === "disabled" ? [] : [{ role: Role.GAMEPAD, mode: OUTPUT[d.gamepadOutput] }],
      outputs: d.roles.includes("gamepad") || d.gamepadOutput !== "disabled" ? [this.#gamepad(d)] : [],
    });
  }

  #wrapper(s: FakeSetting, connected: boolean) {
    const error = s.error ?? (s.state === "unsupported" ? ErrorCode.UNSUPPORTED : null);
    const status = s.saved === null ? undefined
      : { result: error !== null ? { case: "error" as const, value: errorOf(error) } : { case: "state" as const, value: upper(SettingState, s.state as Exclude<FakeSetting["state"], "unsupported">) } };
    // A disconnected device's values haven't been read on this connection.
    const value = connected && s.value !== null ? wireOf(s, s.value) : undefined;
    const saved = s.saved === null ? undefined : wireOf(s, s.saved);
    if (s.type === "enum") return { value, saved, status, choices: s.choices.map((c) => wireOf(s, c)) };
    if (s.type === "bool") return { value, saved, status };
    const limits = s.choices.length
      ? { case: "choices" as const, value: { values: s.choices.map((c) => BigInt(c as number)) } }
      : s.min !== null && s.max !== null ? { case: "range" as const, value: { min: BigInt(s.min), max: BigInt(s.max), step: BigInt(s.step ?? 0) } } : undefined;
    return { value, saved, status, limits };
  }

  /** The settings objects the adapter knows, in listing order: the fields read on the current
   * connection and every saved one. */
  settingsOf(d: FakeDevice): SettingsObject[] {
    const connected = d.state === "connected";
    const known = connected ? d.settings : d.settings.filter((s) => s.saved !== null);
    const objects = new Map<string, Record<string, unknown>>();
    for (const s of known) {
      const id = `${s.group}/${s.index}`;
      const value = objects.get(id) ?? (s.group === "pointer_sensor" ? { index: s.index } : {});
      objects.set(id, value);
      const path = (FIELDS[s.group] as Record<string, { path: string[] }>)[s.field]!.path;
      let node = value;
      for (const k of path.slice(0, -1)) node = (node[k] ??= {}) as Record<string, unknown>;
      node[path.at(-1)!] = this.#wrapper(s, connected);
    }
    return [...objects].map(([id, value]) => create(SettingsObjectSchema, {
      integration: IntegrationKind.HIDPP,
      group: { case: CASES[id.split("/")[0] as SettingsGroupName], value },
    } as MessageInitShape<typeof SettingsObjectSchema>)).sort((a, b) => compareSettingsRefs(settingsRef(a)!, settingsRef(b)!));
  }

  /** Reports the settings objects of `d` that changed, appeared or went away since the client
   * last listed or heard of them. */
  #settingsChanged(d: FakeDevice) {
    const seen = this.#seenSettings.get(d.id) ?? new Map<string, string>();
    const current = this.settingsOf(d);
    const id = (o: SettingsObject) => JSON.stringify(settingsRef(o));
    const now = new Map(current.map((o) => [id(o), hex(toBinary(SettingsObjectSchema, o))]));
    const changed = current.filter((o) => seen.get(id(o)) !== now.get(id(o)));
    const removed = [...seen.keys()].filter((k) => !now.has(k)).map((k) => JSON.parse(k) as { integration: number; group: SettingsGroup; index?: number });
    this.#seenSettings.set(d.id, now);
    if (changed.length || removed.length) this.#event({ case: "settingsChanged", value: { device: d.id, changed, removed } });
  }

  /** Capabilities advertised by this board, qualified by rule kind and collection. */
  #ranges() {
    return [
      ...REMAP_INPUTS,
      ...SCALE_INPUTS,
      ...REMAP_OUTPUTS,
      ...(this.gamepadOutputs ? [
        ...GAME_PAD_OUTPUTS,
        ...GAME_PAD_AXES.flatMap((r) => [UsageRange_Kind.AXIS_INPUT, UsageRange_Kind.AXIS_OUTPUT].map((kind) => ({ ...r, kind }))),
      ] : []),
    ];
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

  /** Replaces what a device reports; the device event carries it. */
  changeInfo(id: number, info: FakeDevice["info"]) {
    const d = this.find(id);
    d.info = info;
    this.#event({ case: "device", value: this.record(d) });
  }

  changeAdapter(patch: { ready?: boolean; storageFull?: boolean; memoryBudget?: number; storageUsed?: number | null; storageTotal?: number | null }) {
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

  /** Reports a changed device, its profiles loading or unloading, and its settings when its
   * connection or HID++ came up or went down. `loads` is what was loaded before the change. */
  #changed(d: FakeDevice, before: { active: boolean; connected: boolean }, loads = this.#loads()) {
    this.#load(loads, d);
    this.#event({ case: "device", value: this.record(d) });
    const connected = d.state === "connected";
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
        d.profileError = ErrorCode.PROFILE_LOAD_FAILED;
        continue;
      }
      const next = new Map(this.#loaded).set(d.id, want);
      if (this.memoryUsed(next) > this.memoryBudget) d.profileError = ErrorCode.PROFILE_MEMORY_EXCEEDED;
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
        if (s.fails !== null) s.error = s.fails;
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
      if (failure !== undefined) throw new Refusal(failure);
      this.#run(command);
    } catch (error) {
      if (!(error instanceof Refusal)) throw error;
      const e = error.error;
      const unknown = this.uncertainWrites && e.code === ErrorCode.STORAGE_FAILED && e.outcome === undefined;
      this.#reply({ result: { case: "error", value: unknown ? { ...e, outcome: ErrorOutcome.UNKNOWN } : e } });
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
        timers.push(setTimeout(() => this.#stopScan(found.length), this.scanError ? 30 * (found.length + 1) : (seconds || 10) * 1000));
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
        if (this.storageFull) throw new Refusal(ErrorCode.NO_CAPACITY, ErrorReason.STORAGE);
        const pairing = { candidate, prompt: false };
        this.#pairing = pairing;
        this.#reply({});
        this.#pairingEvent({ case: "connecting", value: {} });
        const method = this.pairingMethod;
        const failure = this.pairingError;
        this.pairingError = null;
        setTimeout(() => {
          if (this.#pairing !== pairing) return;
          if (failure) {
            this.#pairing = null;
            return this.#pairingEvent({ case: "error", value: failure }, candidate.id);
          }
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
          this.#pairingEvent({ case: "error", value: { code: ErrorCode.REJECTED, outcome: ErrorOutcome.NO_EFFECT } }, p.candidate.id);
        } else setTimeout(() => this.#pairing === p && this.#paired(), 20);
        return;
      }
      case "cancelPairing": {
        const p = this.#pairing;
        this.#pairing = null;
        this.#reply({});
        if (p) this.#pairingEvent({ case: "error", value: { code: ErrorCode.CANCELLED, outcome: ErrorOutcome.NO_EFFECT } }, p.candidate.id);
        return;
      }
      case "listDevices": {
        const listed = sorted(this.devices, (d) => d.id, (a, b) => a - b).map((d) =>
          this.newerDevices.has(d.id) ? { entry: { case: undefined } }
            : this.unreadableDevices.has(d.id) ? { entry: { case: "error" as const, value: { id: d.id, error: READ_ERROR } } } : { entry: { case: "device" as const, value: this.record(d) } });
        return this.#list(command.case, listed, (entries) => ({ case: "devices", value: { entries } }));
      }
      case "getDevice": {
        const d = this.find(command.value.device);
        if (this.unreadableDevices.has(d.id)) throw new Refusal(READ_ERROR);
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
        if (inactive === InactiveReason.CAPACITY) throw new Refusal(ErrorCode.NO_CAPACITY, ErrorReason.ENABLED_DEVICES);
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
        this.#slots.delete(d.id);
        this.#reply({});
        this.#event({ case: "deviceRemoved", value: { id: d.id } });
        this.#load();
        this.#reallocate();
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
      case "listSettings": {
        const d = this.find(command.value.device);
        const objects = sorted(this.settingsOf(d), (o) => settingsRef(o)!, compareSettingsRefs);
        const seen = this.#seenSettings.get(d.id) ?? new Map<string, string>();
        for (const o of objects) seen.set(JSON.stringify(settingsRef(o)), hex(toBinary(SettingsObjectSchema, o)));
        this.#seenSettings.set(d.id, seen);
        return this.#list(command.case, objects, (batch) => ({ case: "settings", value: { device: d.id, objects: batch } }));
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
        return this.#list(command.case, [], (features) => ({ case: "features", value: { features } }));
      case "listFiles":
        return this.#list(command.case, [], (entries) => ({ case: "files", value: { entries } }));
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
    const interfacesChanged = JSON.stringify(interfaces) !== JSON.stringify(this.interfaces);
    const changed = next !== this.name || (platform !== undefined && Platform[platform]!.toLowerCase() !== this.platform) || toggled.length > 0 || interfacesChanged;
    this.interfaces = interfaces;
    this.name = next;
    if (platform !== undefined) this.platform = Platform[platform]!.toLowerCase() as FakeAdapter["platform"];
    this.#reply({});
    if (changed) this.#event({ case: "adapter", value: this.status() });
    for (const t of toggled) this.#applyTransport(t);
    this.#reallocate();
  }

  #setDevice(update: Command<"setDevice">) {
    const d = this.find(update.device);
    const { enabled, trusted, blocked, integrations } = update;
    const kinds = integrations.map((i) => i.kind);
    if (kinds.some((k) => k !== IntegrationKind.HIDPP)) throw new Refusal(ErrorCode.UNSUPPORTED);
    // Output updates apply in order, with the last one for a role winning.
    let gamepadOutput = d.gamepadOutput;
    for (const o of update.outputPreferences) {
      const mode = (Object.keys(OUTPUT) as OutputModeName[]).find((m) => OUTPUT[m] === o.mode);
      if (o.role === Role.UNKNOWN || !(o.role in Role) || !mode) throw new Refusal(ErrorCode.BAD_ARGS);
      if (o.role !== Role.GAMEPAD || (mode !== "disabled" && !this.gamepadOutputs)) throw new Refusal(ErrorCode.UNSUPPORTED);
      gamepadOutput = mode;
    }
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
      && (!layers || (layers.length === d.profiles.length && layers.every((id, i) => id === d.profiles[i])))
      && gamepadOutput === d.gamepadOutput;
    if (same) return this.#reply({});
    // As the firmware does, a change that leaves an unused device enabled and not blocked needs a place.
    const unused = [InactiveReason.DISABLED, InactiveReason.BLOCKED, InactiveReason.CAPACITY].includes(this.inactive(d)!);
    if (unused && (enabled ?? d.enabled) && !(blocked ?? d.blocked)) {
      const used = this.devices.filter((x) => x !== d && x.transport === d.transport && this.inactive(x) === undefined).length;
      if (used >= this.maxEnabled) throw new Refusal(ErrorCode.NO_CAPACITY, ErrorReason.ENABLED_DEVICES);
    }
    const patch: Partial<FakeDevice> = {};
    if (enabled !== undefined) patch.enabled = enabled;
    if (trusted !== undefined) patch.trusted = trusted;
    if (blocked !== undefined) patch.blocked = blocked;
    if (layers) patch.profiles = [...layers];
    patch.gamepadOutput = gamepadOutput;
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
    this.#reallocate();
  }

  #setSettings({ device: id, changes }: Command<"setSettings">) {
    const d = this.find(id);
    if (!changes.length) throw new Refusal(ErrorCode.BAD_ARGS);
    // Every change is checked before any is saved. Saved fields are known while disconnected.
    const known = d.state === "connected" ? d.settings : d.settings.filter((s) => s.saved !== null);
    const checked = changes.flatMap((u) => {
      if (u.integration !== IntegrationKind.HIDPP) throw new Refusal(ErrorCode.NOT_FOUND);
      const fields = updateChanges(u);
      if (!fields.length) throw new Refusal(ErrorCode.BAD_ARGS);
      return fields.map((c) => {
        const f = c.setting;
        const s = known.find((x) => sameObject(x, f.group, f.index) && x.field === f.field);
        if (!s) throw new Refusal(ErrorCode.NOT_FOUND);
        if (c.type === "forget") return { s, value: null };
        if (!takes(s, c.value)) throw new Refusal(ErrorCode.BAD_ARGS);
        return { s, value: c.value };
      });
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
        const listed = sorted(this.profiles, (p) => p.id, (a, b) => a - b).map((p) =>
          this.newerProfiles.has(p.id) ? { entry: { case: undefined } }
            : this.unreadableProfiles.has(p.id) ? { entry: { case: "error" as const, value: { id: p.id, error: READ_ERROR } } } : { entry: { case: "profile" as const, value: this.#profileRecord(p) } });
        return this.#list(command.case, listed, (entries) => ({ case: "profiles", value: { entries } }));
      }
      case "getProfile": {
        const p = this.#profile(command.value.profile);
        if (this.unreadableProfiles.has(p.id)) throw new Refusal(READ_ERROR);
        return this.#reply({ result: { case: "profile", value: this.#profileRecord(p) } });
      }
      case "createProfile":
      case "copyProfile": {
        const source = command.case === "copyProfile" ? this.#profile(command.value.profile) : null;
        const { name } = command.value;
        if (adapterName(name) === null) throw new Refusal(ErrorCode.BAD_ARGS);
        this.#needReady();
        if (this.storageFull) throw new Refusal(ErrorCode.NO_CAPACITY, ErrorReason.STORAGE);
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
        const rules = sorted(p.rules, ruleRef, compareRuleRefs);
        return this.#list(command.case, rules, (batch) => ({ case: "profileRules", value: { profile: p.id, rules: batch } }));
      }
      case "setProfileRules": {
        const p = this.#profile(command.value.profile);
        if (!command.value.changes.length) throw new Refusal(ErrorCode.BAD_ARGS);
        const rules = [...p.rules];
        for (const change of command.value.changes) {
          const c = change.change;
          if (c.case === undefined) throw new Refusal(ErrorCode.BAD_ARGS);
          const { input, collection } = c.value;
          if (!input || !collection) throw new Refusal(ErrorCode.BAD_ARGS);
          const i = rules.findIndex((r) => sameUsage(r.input, input) && sameUsage(r.collection, collection));
          if (i !== -1) rules.splice(i, 1);
          if (c.case === "forget") continue;
          const effect = c.value.effect;
          if (effect.case === undefined) throw new Refusal(ErrorCode.BAD_ARGS);
          const ranges = this.#ranges();
          const kind = { remap: UsageRange_Kind.REMAP_INPUT, scale: UsageRange_Kind.SCALE_INPUT, axis: UsageRange_Kind.AXIS_INPUT }[effect.case];
          if (!within(ranges, kind, input, collection)) throw new Refusal(ErrorCode.BAD_ARGS);
          if (effect.case === "remap") {
            if (effect.value.outputs.length > 8) throw new Refusal(ErrorCode.BAD_ARGS);
            // An output without a collection takes the one collection that sends it.
            for (const o of effect.value.outputs) {
              const found = ranges.filter((r) => (!o.collection || sameUsage(r.collection, o.collection)) && within([r], UsageRange_Kind.REMAP_OUTPUT, o.usage, r.collection));
              if (!found.length || found.some((r) => !sameUsage(r.collection, found[0]!.collection))) throw new Refusal(ErrorCode.BAD_ARGS);
              o.collection = create(UsageSchema, found[0]!.collection);
            }
          }
          if (effect.case === "scale" && (effect.value.numerator === 0 || effect.value.denominator === 0)) throw new Refusal(ErrorCode.BAD_ARGS);
          if (effect.case === "axis" && !within(ranges, UsageRange_Kind.AXIS_OUTPUT, effect.value.destination, collection)) throw new Refusal(ErrorCode.BAD_ARGS);
          // A rule that changes nothing is forgotten instead.
          const only = effect.case === "remap" && effect.value.outputs.length === 1 ? effect.value.outputs[0]! : undefined;
          const identity = effect.case === "remap"
            ? !!only && sameUsage(only.usage, input) && sameUsage(only.collection, collection)
            : effect.case === "scale"
              ? effect.value.numerator === effect.value.denominator
              : sameUsage(effect.value.destination, input) && !effect.value.invert;
          if (!identity) rules.push(create(ProfileRuleSchema, c.value));
        }
        const grown = { ...p, rules };
        const loaded = [...this.#loaded.values()].some((ids) => ids.includes(p.id));
        const after = loaded ? this.memoryUsed() - sizeOf(p) + sizeOf(grown) : sizeOf(grown);
        if (sizeOf(grown) > this.memoryBudget || after > this.memoryBudget) throw new Refusal(ErrorCode.NO_CAPACITY, ErrorReason.PROFILE_MEMORY);
        this.#needReady();
        const roles = rolesOf(p).join();
        const loads = this.#loads();
        p.rules = rules.sort((a, b) => compareRuleRefs(ruleRef(a), ruleRef(b)));
        this.#reply({});
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
        this.#pairingEvent({ case: "error", value: { code: ErrorCode.UNSUPPORTED, outcome: ErrorOutcome.NO_EFFECT } }, p.candidate.id);
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
    const error = this.scanError ?? undefined;
    this.scanError = null;
    this.#event({ case: "scanDone", value: { count, error } });
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
      info: { battery: { level: 90 } },
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
