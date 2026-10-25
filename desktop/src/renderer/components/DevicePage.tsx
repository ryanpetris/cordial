import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { isLow } from "../../shared/battery.ts";
import { enabledFull } from "../../shared/capacity.ts";
import { gamepadModes, gamepadOutput, offersGamepadOutput } from "../../shared/outputs.ts";
import { fieldOf, settingId, settingsBusy, settingsCurrent } from "../../shared/settings.ts";
import type {
  AdapterEntry,
  AppState,
  DeviceChanges,
  DeviceEntry,
  DeviceRecord,
  OutputModeName,
  Scalar,
  Setting,
  SettingsChange,
  SettingsSaveItem,
} from "../../shared/state.ts";
import {
  inactiveText,
  OUTPUT_MODES,
  OUTPUT_STATES,
  READ_FAILED,
  ROLES,
  SETTING_STATES,
  TRANSPORTS,
  batteryStale,
  batteryText,
  choiceText,
  connected as isConnected,
  connectionError,
  deviceStatus,
  displayUnit,
  errorText,
  fieldName,
  identifierFacts,
  identityFacts,
  integrationText,
  integrationUnsupported,
  kindText,
  neverValue,
  profileError,
  profileErrorText,
  readings as readingsOf,
  reasonText,
  securityFacts,
  settingInfo,
  settingOrder,
  stateText,
  unsupportedError,
  versionText,
  wheelFigures,
  type FieldName,
  type FieldRef,
  type Reading,
} from "../../shared/text.ts";
import { useAction } from "../api.ts";
import { DeviceLayers } from "./Profiles.tsx";
import { Banner, Card, Dialog, Fact, Facts, Menu, Page, Pill, Row, Segmented, Spinner, Staged, Switch, SwitchRow, TabBar, TabPanel, type MenuOption } from "./common.tsx";
import {
  BatteryGlyph,
  CheckIcon,
  CloseIcon,
  DeviceIcon,
  PlugIcon,
  RefreshIcon,
  StateMark,
  TrashIcon,
  UndoIcon,
  UnplugIcon,
  type MarkShape,
} from "./icons.tsx";

type DeviceTab = "details" | "settings" | "profiles" | "diagnostics";
const DEVICE_TABS: [DeviceTab, string][] = [
  ["details", "Details"],
  ["settings", "Settings"],
  ["profiles", "Profiles"],
  ["diagnostics", "Diagnostics"],
];
/** A setting's field, as the settings tables name it. */
const ref = (s: Setting): FieldRef => ({ name: fieldName(s), index: s.index });

const label = (s: Setting) => settingInfo(ref(s)).label;

function valueText(name: FieldName, v: Scalar | null): string {
  if (v === null) return "Unknown";
  if (typeof v === "boolean") return v ? "On" : "Off";
  if (typeof v === "string") return choiceText(name, v);
  if (neverValue(name, v)) return "Never";
  return String(v);
}

const choiceLabel = (s: Setting, c: Scalar) => (typeof c === "string" ? choiceText(fieldName(s), c) : String(c));

/** Whether a setting's choices fit side by side as joined buttons. */
const short = (s: Setting) => s.choices.length <= 3 && s.choices.reduce<number>((n, c) => n + choiceLabel(s, c).length, 0) <= 24;

/** A staged change. A `policy` save comes from Save Current Value or Save
 * Device Value and is a change even when it matches the shown value. */
export type Draft = { type: "set"; value: Scalar; policy?: boolean } | { type: "forget" };

/** A device's staged changes. `set` with `expected` changes only a draft still equal to it. */
export interface Drafts {
  get(setting: string): Draft | undefined;
  set(setting: string, draft: Draft | undefined, expected?: Draft): void;
  clear(): void;
}

const freeform = (s: Setting) => s.type === "integer" && !s.choices.length;

/** Whether the row shows the reading as text instead of a control: an enum without choices takes
 * no value. */
const textOnly = (s: Setting) => s.type === "enum" && !s.choices.length;

/** Whether the setting takes `v`. */
function accepts(s: Setting, v: Scalar | null): v is Scalar {
  if (v === null) return false;
  if (s.type === "bool") return typeof v === "boolean";
  if (s.choices.length) return s.choices.includes(v);
  if (s.type !== "integer") return typeof v === "string";
  return typeof v === "number" && Number.isSafeInteger(v) && (s.min == null || v >= s.min) && (s.max == null || v <= s.max)
    && (v - (s.min ?? 0)) % (s.step ?? 1) === 0;
}

const freeSpin = (s: Setting) => s.group === "wheel" && s.field === "free_spin_threshold";

/** The range typed text must be in: an automatic free spin threshold is 1-254, since only its
 * toggle turns it off with 255. */
const typedRange = (s: Setting): Setting => (freeSpin(s) ? { ...s, max: Math.min(s.max ?? 254, 254) } : s);

/** The value a set draft saves, or null when it isn't one the setting takes. */
function draftValue(s: Setting, value: Scalar): Scalar | null {
  if (!freeform(s)) return accepts(s, value) ? value : null;
  const n = Number(value);
  return String(value).trim() !== "" && accepts(typeof value === "string" ? typedRange(s) : s, n) ? n : null;
}

const saved = (s: Setting) => s.saved !== null;

/** The value the device keeps when nothing is staged: the saved value, else the reading. */
const baseValue = (s: Setting) => (saved(s) ? s.saved : s.value);

/** A draft's change to submit; null when it matches what is saved or it is invalid. */
function pendingChange(s: Setting, draft: Draft | undefined): { change: SettingsChange | null; invalid: boolean } {
  if (!draft) return { change: null, invalid: false };
  if (draft.type === "forget") return { change: saved(s) ? { type: "forget", setting: fieldOf(s) } : null, invalid: false };
  const value = draftValue(s, draft.value);
  if (value === null) return { change: null, invalid: true };
  if (!draft.policy && value === baseValue(s)) return { change: null, invalid: false };
  return { change: { type: "set", setting: fieldOf(s), value }, invalid: false };
}

/** Whether a row shows a current reading rather than the last one. */
const settingFresh = (entry: DeviceEntry, s: Setting) => settingsCurrent(entry) && s.value !== null;

/** The form's shared guards: whether values can be edited, and whether staging and the footer can act. */
interface FormGuards {
  /** Value controls are disabled. */
  locked: boolean;
  /** Nothing can be staged, saved or refreshed. */
  busy: boolean;
}

/** The fixed-width state marker; writable rows open their staging menu from it. */
function Marker({ label, state, shape, options, sending, disabled }: {
  label: string;
  state: string;
  shape: MarkShape;
  options: MenuOption[];
  sending: boolean;
  disabled: boolean;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const name = `${label}, ${state}, Options`;
  if (sending)
    return (
      <span className="marker" role="img" aria-label={`${label}, Sending`} aria-busy="true">
        <Spinner />
      </span>
    );
  return (
    <>
      <button
        ref={button}
        type="button"
        className={`marker icon-button ${shape}`}
        aria-label={name}
        aria-haspopup="menu"
        aria-expanded={!!menu}
        disabled={disabled}
        onClick={() => {
          const r = button.current!.getBoundingClientRect();
          setMenu(menu ? null : { x: r.right, y: r.bottom });
        }}
      >
        <StateMark shape={shape} />
      </button>
      {menu ? (
        <Menu
          label={name}
          x={menu.x}
          y={menu.y}
          options={options}
          onClose={(refocus) => {
            setMenu(null);
            if (refocus) button.current?.focus();
          }}
        />
      ) : null}
    </>
  );
}

/** Stages `v` from a value control. Choosing or typing what the device keeps
 * anyway drops the draft, except that a staged save of an unsaved value stays one. */
function edit(s: Setting, drafts: Drafts, v: Scalar) {
  const id = settingId(s);
  const current = drafts.get(id);
  const policy = !saved(s) && current?.type === "set" && !!current.policy;
  const next = draftValue(s, v);
  if (!policy && next !== null && next === baseValue(s)) drafts.set(id, undefined);
  else drafts.set(id, { type: "set", value: v, ...(policy ? { policy } : {}) });
}

/** The row's value control, showing the draft, else the saved value, else the reading. */
function Control({ s, draft, guards, drafts, invalid }: { s: Setting; draft: Draft | undefined; guards: FormGuards; drafts: Drafts; invalid: boolean }) {
  const name = label(s);
  const value: Scalar | null = draft?.type === "set" ? draft.value : draft?.type === "forget" ? s.value : baseValue(s);
  const change = (v: Scalar) => edit(s, drafts, v);
  if (textOnly(s)) return <span className="value">{valueText(fieldName(s), s.value)}</span>;
  if (s.type === "bool")
    return <Switch label={name} checked={typeof value === "boolean" ? value : null} disabled={guards.locked} onChange={change} />;
  if (freeSpin(s) && freeform(s)) return <FreeSpin s={s} value={value} guards={guards} drafts={drafts} invalid={invalid} />;
  if (s.choices.length && value !== null && s.choices.includes(value) && short(s))
    return (
      <Segmented
        label={name}
        options={s.choices.map((c) => [c, choiceLabel(s, c)])}
        value={value}
        disabled={guards.locked}
        onChange={change}
      />
    );
  if (s.choices.length)
    return (
      <select
        aria-label={name}
        value={value === null ? "" : String(value)}
        disabled={guards.locked}
        onChange={(e) => {
          const choice = s.choices.find((c) => String(c) === e.target.value);
          if (choice !== undefined) change(choice);
        }}
      >
        {value === null || !s.choices.includes(value) ? (
          <option value={value === null ? "" : String(value)} disabled>
            {valueText(fieldName(s), value)}
          </option>
        ) : null}
        {s.choices.map((c) => (
          <option key={String(c)} value={String(c)}>
            {choiceLabel(s, c)}
          </option>
        ))}
      </select>
    );
  return (
    <IntegerControl
      s={s}
      text={typeof value === "string" || typeof value === "number" ? String(value) : ""}
      invalid={invalid}
      disabled={guards.locked}
      label={name}
      onEdit={change}
      onRevert={draft ? () => drafts.set(settingId(s), undefined) : null}
    />
  );
}

/** Automatic free spin: on with a threshold of 1-254, or off, which the device takes as 255. */
function FreeSpin({ s, value, guards, drafts, invalid }: { s: Setting; value: Scalar | null; guards: FormGuards; drafts: Drafts; invalid: boolean }) {
  // Text typed into the threshold keeps it On, even while empty or invalid.
  const on = typeof value === "string" ? true : typeof value === "number" ? value !== 255 : null;
  const threshold = [s.saved, s.value].find((v): v is number => typeof v === "number" && v >= 1 && v <= 254) ?? 254;
  return (
    <>
      <Switch label="Automatic Free Spin" checked={on} disabled={guards.locked} onChange={(v) => edit(s, drafts, v ? threshold : 255)} />
      {on ? (
        <IntegerControl
          s={typedRange(s)}
          text={String(value)}
          invalid={invalid}
          disabled={guards.locked}
          label={label(s)}
          onEdit={(v) => edit(s, drafts, v)}
          onRevert={drafts.get(settingId(s)) ? () => drafts.set(settingId(s), undefined) : null}
        />
      ) : null}
    </>
  );
}

function IntegerControl({
  s,
  text,
  invalid,
  disabled,
  label,
  onEdit,
  onRevert,
}: {
  s: Setting;
  text: string;
  invalid: boolean;
  disabled: boolean;
  label: string;
  onEdit: (text: string) => void;
  /** Escape undoes the row's draft; null when there is none. */
  onRevert: (() => void) | null;
}) {
  const step = s.step ?? 1;
  const min = s.min ?? undefined;
  const max = s.max ?? undefined;
  const slider = min !== undefined && max !== undefined && (max - min) / step <= 50;
  return slider ? (
    <span className="slider">
      <input type="range" aria-label={label} min={min} max={max} step={step} value={text || min} disabled={disabled} onChange={(e) => onEdit(e.target.value)} />
      <output>{text}</output>
    </span>
  ) : (
    <input
      className="number"
      type="number"
      aria-label={label}
      aria-invalid={invalid}
      min={min}
      max={max}
      step={step}
      value={text}
      disabled={disabled}
      onChange={(e) => onEdit(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Escape" && onRevert) {
          e.preventDefault();
          e.stopPropagation();
          onRevert();
        }
      }}
    />
  );
}

/** A range as the row states it when a typed value is outside it. */
function rangeText(s: Setting): string {
  const range = s.min != null && s.max != null ? `${s.min}-${s.max}` : s.min != null ? `At Least ${s.min}` : s.max != null ? `At Most ${s.max}` : "Whole Numbers";
  return s.step && s.step > 1 ? `${range}, Steps of ${s.step}` : range;
}

/** One setting: label, value control, unit and state marker. */
function SettingRow({ entry, s, drafts, guards, item }: {
  entry: DeviceEntry;
  s: Setting;
  drafts: Drafts;
  guards: FormGuards;
  /** This setting's outcome in the latest submission. */
  item: SettingsSaveItem | undefined;
}) {
  const fresh = settingFresh(entry, s);
  const id = settingId(s);
  const info = settingInfo(ref(s));
  // Text readings follow their value; editors keep the unit beside the number.
  const unit = textOnly(s) ? displayUnit(fieldName(s), s.value) : info.unit;
  const draft = drafts.get(id);
  const { change, invalid } = pendingChange(s, draft);
  const staged = change !== null || invalid;
  const settable = fresh && accepts(s, s.value);
  const forget: MenuOption = ["Forget Saved Value", () => drafts.set(id, { type: "forget" })];
  const keep = (text: string): MenuOption => [text, settable ? () => drafts.set(id, { type: "set", value: s.value!, policy: true }) : null];
  let shape: MarkShape;
  let state: string;
  let options: MenuOption[];
  if (staged) {
    [shape, state, options] = ["draft", "Changed", [["Undo Change", () => drafts.set(id, undefined)]]];
  } else if (!saved(s)) {
    [shape, state, options] = ["outline", SETTING_STATES.unmanaged, [keep("Save Current Value")]];
  } else if (s.state === "changed_on_device") {
    [shape, state, options] = ["differs", SETTING_STATES.changed_on_device, [keep("Save Device Value"), forget]];
  } else if (s.error) {
    [shape, state, options] = ["problem", SETTING_STATES[s.error.code === "unsupported" ? "unsupported" : "error"], [forget]];
  } else {
    [shape, state, options] = ["filled", SETTING_STATES[s.state ?? "pending"], [forget]];
  }

  const notes: ReactNode[] = [];
  if (item?.status === "not_saved") notes.push(<span key="save" className="error-text">Couldn't Save{item.error ? `: ${reasonText(item.error)}` : ""}</span>);
  else if (item?.status === "unknown" && item.error) notes.push(<span key="save" className="error-text">{reasonText(item.error)}</span>);
  if (invalid) notes.push(<span key="range" className="error-text">{rangeText(typedRange(s))}</span>);
  if (saved(s) && s.state === "changed_on_device" && fresh) notes.push(<span key="device">Device: {valueText(fieldName(s), s.value)}</span>);
  if (s.error && !unsupportedError(s.error)) notes.push(<span key="error">{reasonText(errorText(s.error, "apply"))}</span>);

  const labelId = `setting-${id}`;
  return (
    <div className={fresh ? "setting-row" : "setting-row stale"} role="group" aria-labelledby={labelId}>
      <div className="setting-label">
        <div className="row-title" id={labelId}>
          {info.label}
        </div>
        {notes.length ? <div className="row-subtitle">{notes.flatMap((n, i) => (i ? [" · ", n] : [n]))}</div> : null}
      </div>
      <fieldset className="setting-control" disabled={guards.locked}>
        <Control s={s} draft={draft} guards={guards} drafts={drafts} invalid={invalid} />
        {unit ? <span className="unit">{unit}</span> : null}
      </fieldset>
      <div className="setting-marker">
        <Marker label={info.label} state={state} shape={shape} options={options} sending={item?.status === "saving"} disabled={guards.busy} />
      </div>
    </div>
  );
}

/** A value the device only reports, shown with the settings it belongs to. */
function ReadingRow({ f, dim }: { f: Reading; dim: boolean }) {
  const info = settingInfo(f);
  const unit = displayUnit(f.name, f.value);
  return (
    <div className={dim ? "setting-row stale" : "setting-row"} role="group" aria-label={info.label}>
      <div className="setting-label">
        <div className="row-title">{info.label}</div>
      </div>
      <div className="setting-control">
        <span className="value">{valueText(f.name, f.value)}</span>
        {unit ? <span className="unit">{unit}</span> : null}
      </div>
      <div className="setting-marker" />
    </div>
  );
}

/** The settings tab's contents: settings the page knows, readings shown with them, and the wheel's figures. */
function settingsView(entry: DeviceEntry) {
  const rows = [...(entry.settings ?? [])].sort((a, b) => settingOrder(ref(a)) - settingOrder(ref(b)));
  // A field the device takes writes for shows as a setting rather than a reading.
  const fields = new Set(rows.map((s) => `${fieldName(s)}/${s.index}`));
  const readings = readingsOf(entry.device.information).filter((f) => !fields.has(`${f.name}/${f.index}`)).sort((a, b) => settingOrder(a) - settingOrder(b));
  const figures = wheelFigures(entry.device.information);
  return { rows, readings, figures };
}

/** Whether the device page has a settings tab: only once the device has settings to show. Saved
 * settings stay visible while disconnected. */
function hasSettings(entry: DeviceEntry) {
  const { rows, readings, figures } = settingsView(entry);
  return rows.length > 0 || readings.length > 0 || figures.length > 0;
}

/** HID++ is coming up on the connected device, reading its settings. */
const starting = (entry: DeviceEntry) => isConnected(entry) && entry.device.hidpp?.state === "starting";

/** The settings form: every staged change goes to the device together with
 * Save. Its buttons go in the page bar, `bar`, outside the form. */
function Settings({ entry, live, adapter, drafts, details, onDetails, bar }: {
  entry: DeviceEntry;
  /** False while the page shows the device's last state as its adapter reconnects. */
  live: boolean;
  adapter: AdapterEntry | undefined;
  drafts: Drafts;
  details: DeviceChanges;
  onDetails: (update: (draft: DeviceChanges) => DeviceChanges) => void;
  bar: HTMLElement | null;
}) {
  const formId = useId();
  const [refreshing, runRefresh] = useAction(true);
  const [saving, runSave] = useAction(true);
  const [reloading, runReload] = useAction(true);
  const [problem, setProblem] = useState<string | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const d = entry.device;
  const status = adapter?.status ?? null;
  const gamepad = offersGamepadOutput(d, status);
  const savedOutput = gamepadOutput(d);
  // A saved mode the adapter no longer takes stays shown, so it can be turned off.
  const available = gamepadModes(status);
  const outputModes = (Object.keys(OUTPUT_MODES) as OutputModeName[]).filter((m) => m === savedOutput || available.includes(m));
  const outputChange = gamepad ? pendingDetails(details, d).gamepadOutput : undefined;
  const stageOutput = (mode: OutputModeName | undefined) => onDetails((x) => pendingDetails({ ...x, gamepadOutput: mode }, d));
  const { rows: settings, readings, figures } = settingsView(entry);
  const categories = [...new Set([...settings.map(ref), ...readings].sort((a, b) => settingOrder(a) - settingOrder(b)).map((f) => settingInfo(f).category))];
  if (figures.length && !categories.includes("Wheel")) categories.push("Wheel");
  // A record that is only last known leaves Refresh to the adapter.
  const connected = isConnected(entry) || !!entry.readError;
  const reachable = live && adapter?.connection === "connected";
  const occupied = saving || refreshing || settingsBusy(entry);
  // Saving needs the adapter ready; refreshing a connected device doesn't.
  const busy = !reachable || !adapter.status?.ready || occupied;
  const guards: FormGuards = { locked: busy, busy };
  const submission = entry.settingsSave;
  const items = submission?.items ?? [];
  const itemFor = (id: string) => items.find((i) => settingId(i.change.setting) === id);
  const staged = settings.map((s) => ({ s, draft: drafts.get(settingId(s)), ...pendingChange(s, drafts.get(settingId(s))) }));
  const changes = staged.filter((r) => r.change !== null);
  const dirty = staged.some((r) => r.change !== null || r.invalid) || outputChange !== undefined;
  const invalid = staged.some((r) => r.invalid);
  const canSave = !busy && (changes.length > 0 || outputChange !== undefined) && !invalid;
  // Retry saves a failed setting's value again, which applies it again; a
  // draft of that setting stands in its way.
  const retryable = staged.filter((r) => r.s.error && r.s.error.code !== "unsupported" && saved(r.s));
  const retry = retryable.filter((r) => !r.change && !r.invalid);
  const canRetry = !busy && retry.length > 0;

  const submit = async (list: SettingsChange[], cohort: [string, Draft | undefined][]) => {
    setProblem(null);
    const result = await runSave({ type: "settings.save", key: entry.key, changes: list });
    for (const item of result.settingsSave?.items ?? []) {
      const id = settingId(item.change.setting);
      const draft = cohort.find(([key]) => key === id)?.[1];
      if (draft && item.status === "saved") drafts.set(id, undefined, draft);
    }
    if (!result.ok && !result.inline) setProblem(result.message);
    return result.ok;
  };
  // Settings go first, since a Gamepad Output change can reconnect USB. A setting that wasn't
  // saved, or may not have been, keeps the output change staged.
  const save = async () => {
    if (!canSave) return;
    setProblem(null);
    if (changes.length && !(await submit(changes.map((r) => r.change!), changes.map((r) => [settingId(r.s), r.draft])))) return;
    const sent = outputChange;
    if (sent === undefined) return;
    const result = await runSave({ type: "device.update", key: entry.key, gamepadOutput: sent });
    if (result.ok) onDetails((x) => settleDetails(x, { gamepadOutput: sent }));
    else setProblem(result.message);
  };
  const requestSave = () => { void save(); };
  const onSubmit = (e: FormEvent) => {
    e.preventDefault();
    requestSave();
  };

  // Ctrl+S or Cmd+S saves while the settings form is shown and no dialog is open.
  const saveRef = useRef(requestSave);
  saveRef.current = requestSave;
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() !== "s" || !(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return;
      if (document.querySelector("dialog[open]")) return;
      e.preventDefault();
      saveRef.current();
    };
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  }, []);

  const loadProblem = (
    <>
      <span className="muted">The adapter couldn't read the device's settings.</span>
      <button type="button" disabled={reloading} onClick={() => void runReload({ type: "device.reload", key: entry.key })}>
        <RefreshIcon /> Retry
      </button>
    </>
  );
  const hidppError = integrationUnsupported(d.hidpp?.error ?? null) ? null : (d.hidpp?.error ?? null);
  // A change whose outcome was unknown no longer is once the device reports it saved.
  useEffect(() => {
    if (outputChange === undefined) setProblem(null);
  }, [outputChange]);

  // The tab is only offered while there are settings.
  if (!settings.length && !readings.length && !figures.length && !gamepad) return null;

  const notSaved = items.filter((i) => i.status === "not_saved").length;
  const unknown = items.find((i) => i.status === "unknown")?.error ?? "";
  const failures = submission && !submission.running ? (notSaved ? `Couldn't Save ${notSaved}` : unknown) : "";
  const note = problem ?? (failures || (hidppError ? errorText(hidppError, "apply") : null));
  const refresh = async () => {
    setProblem(null);
    const result = await runRefresh({ type: "device.refresh", key: entry.key });
    if (!result.ok) setProblem(result.message);
  };
  // Footer buttons stay focusable while unavailable, so a focused Save keeps focus while it runs.
  const guard = (enabled: boolean, run: () => void) => ({
    "aria-disabled": !enabled,
    onClick: () => {
      if (enabled) run();
    },
  });
  const working = saving || refreshing || !!submission?.running || starting(entry);

  const footer = (
    <>
      <span className="bar-start">
        {working ? <Spinner /> : null}
        {note ? <span className="error-text">{note}</span> : null}
        {entry.settingsError ? loadProblem : null}
      </span>
      <button type="button" {...guard(reachable && !occupied && connected, () => void refresh())}>
        <RefreshIcon /> Refresh
      </button>
      {retryable.length ? (
        <button type="button" {...guard(canRetry, () => void submit(retry.map((r) => ({ type: "set", setting: fieldOf(r.s), value: r.s.saved! })), []))}>
          <RefreshIcon /> Retry
        </button>
      ) : null}
      <button type="button" {...guard(!saving && dirty, () => {
        drafts.clear();
        stageOutput(undefined);
      })}>
        <UndoIcon /> Discard
      </button>
      <button type="submit" form={formId} className="suggested" aria-disabled={!canSave}>
        <CheckIcon /> Save
      </button>
    </>
  );

  return (
    <>
      <form
        ref={form}
        id={formId}
        className={busy ? "settings-form busy" : "settings-form"}
        aria-busy={working}
        onSubmit={onSubmit}
        onKeyDown={(e) => {
          const t = e.target as HTMLElement;
          if (e.key === "Enter" && (t.tagName === "INPUT" || t.tagName === "SELECT")) {
            e.preventDefault();
            requestSave();
          }
        }}
      >
        <div className="settings-fields">
          {gamepad ? (
            <Card title={ROLES.gamepad}>
              <div className="setting-row" role="group" aria-labelledby={`${formId}-gamepad`}>
                <div className="setting-label">
                  <div className="row-title" id={`${formId}-gamepad`}>
                    Gamepad Output
                  </div>
                </div>
                <fieldset className="setting-control" disabled={guards.locked}>
                  <Segmented
                    label="Gamepad Output"
                    options={outputModes.map((m) => [m, OUTPUT_MODES[m]])}
                    value={outputChange ?? savedOutput}
                    disabled={guards.locked}
                    onChange={stageOutput}
                  />
                </fieldset>
                <div className="setting-marker">
                  {outputChange !== undefined ? (
                    <Marker
                      label="Gamepad Output"
                      state="Changed"
                      shape="draft"
                      options={[["Undo Change", () => stageOutput(undefined)]]}
                      sending={saving}
                      disabled={guards.busy}
                    />
                  ) : null}
                </div>
              </div>
            </Card>
          ) : null}
          {categories.map((category) => (
            <Card key={category} title={category}>
              {settings
                .filter((s) => settingInfo(ref(s)).category === category)
                .map((s) => (
                  <SettingRow key={settingId(s)} entry={entry} s={s} drafts={drafts} guards={guards} item={itemFor(settingId(s))} />
                ))}
              {readings
                .filter((f) => settingInfo(f).category === category)
                .map((f) => (
                  <ReadingRow key={`${f.name}/${f.index}`} f={f} dim={!connected} />
                ))}
              {category === "Wheel" && figures.length ? (
                <div className={connected ? "figures" : "figures dim"}>
                  {figures.map(([name, value]) => (
                    <span key={name} className="figure">
                      <span className="figure-value">{value}</span>
                      <span className="figure-label">{name}</span>
                    </span>
                  ))}
                </div>
              ) : null}
            </Card>
          ))}
        </div>
      </form>
      {bar ? createPortal(footer, bar) : null}
    </>
  );
}

/** The staged values in `draft` that differ from the device's saved ones. */
function pendingDetails(draft: DeviceChanges, d: DeviceRecord): DeviceChanges {
  const changes: DeviceChanges = {};
  for (const k of ["enabled", "trusted", "blocked"] as const) if (draft[k] !== undefined && draft[k] !== d[k]) changes[k] = draft[k];
  if (draft.hidpp !== undefined && draft.hidpp !== (d.hidpp?.enabled ?? false)) changes.hidpp = draft.hidpp;
  if (draft.profiles !== undefined && d.profiles !== null && draft.profiles.join() !== d.profiles.join()) changes.profiles = draft.profiles;
  if (draft.gamepadOutput !== undefined && draft.gamepadOutput !== gamepadOutput(d)) changes.gamepadOutput = draft.gamepadOutput;
  return changes;
}

/** The tab that stages and saves each field of the Details changes. */
const fieldTab = (field: string): DeviceTab => (field === "profiles" ? "profiles" : field === "gamepadOutput" ? "settings" : "details");

/** The fields of `changes` that `tab` stages. */
const ofTab = (changes: DeviceChanges, tab: DeviceTab): DeviceChanges =>
  Object.fromEntries(Object.entries(changes).filter(([k]) => fieldTab(k) === tab));

/** `draft` without the values `sent` saved; values staged again since stay. */
function settleDetails(draft: DeviceChanges, sent: DeviceChanges): DeviceChanges {
  const next = { ...draft };
  for (const k of Object.keys(sent) as (keyof DeviceChanges)[]) if (String(next[k]) === String(sent[k])) delete next[k];
  return next;
}

export function DevicePage({ state, entry, drafts, details, onDetails }: {
  state: AppState;
  entry: DeviceEntry;
  drafts: Drafts;
  /** The device's staged Details changes, kept by the window across tabs and pages. */
  details: DeviceChanges;
  onDetails: (update: (draft: DeviceChanges) => DeviceChanges) => void;
}) {
  const [busy, run] = useAction(true);
  const [saving, runSave] = useAction(true);
  // A failed save, shown in the bar of the tab whose changes it sent.
  const [saveError, setSaveError] = useState<{ tab: DeviceTab; message: string } | null>(null);
  const [connectBusy, runConnect] = useAction(true);
  const [infoBusy, runInfo] = useAction(true);
  // The last failed action on this page, shown next to the control that ran it.
  const [failure, setFailure] = useState<{ at: string; message: string } | null>(null);
  const [forgetting, setForgetting] = useState(false);
  // A device opens on its details, where Connect is; the tab stays put as the connection changes.
  const [chosen, setTab] = useState<DeviceTab>("details");
  const tabs = useId();
  const [settingsBar, setSettingsBar] = useState<HTMLElement | null>(null);
  const d = entry.device;
  const adapter = state.adapters.find((a) => a.id === entry.adapterId);
  const connected = isConnected(entry);
  const live = state.devices.some((x) => x.key === entry.key);
  const settings = hasSettings(entry) || offersGamepadOutput(d, adapter?.status ?? null);
  useEffect(() => setFailure(null), [d.state]);
  const perform = async (at: string, runner: typeof run, action: Parameters<typeof run>[0]) => {
    setFailure(null);
    const result = await runner(action);
    if (!result.ok) setFailure({ at, message: result.message });
    return result;
  };
  const failed = (at: string) => (failure?.at === at ? failure.message : null);
  const failedText = (at: string) => (failure?.at === at ? <span className="error-text">{failure.message}</span> : undefined);
  const connecting = connectBusy || d.state === "connecting";
  const layered = !!adapter?.status?.profileSupport && d.profiles !== null;
  const tab = (chosen === "settings" && !settings) || (chosen === "profiles" && !layered) ? "details" : chosen;
  const low = isLow(entry.battery, state.preferences.lowBatteryPercent);
  const battery = batteryText(entry.battery);
  const ready = live && adapter?.connection === "connected" && !!adapter.status?.ready;
  // A record that is only last known leaves Connect to the adapter.
  const canConnect = ready && (!!entry.readError || (d.inactive === null && d.state === "disconnected"));
  const peers = state.devices.filter((x) => x.adapterId === entry.adapterId).map((x) => x.device);
  const info = identityFacts(d.information);
  const identifiers = identifierFacts(d.information);
  // Details, Profiles and Gamepad Output edits stage in `details`; each tab's Save sends only its
  // own changes.
  const changes = pendingDetails(details, d);
  const detailChanges = ofTab(changes, "details");
  const layerChanges = changes.profiles;
  // Turning the device on or unblocking it is not offered while the adapter would refuse it for
  // want of a place for its transport.
  const full = (update: DeviceChanges) => enabledFull(adapter?.status ?? null, peers, d, { ...detailChanges, ...update });
  const shown = ofTab(changes, tab);
  const dirty = Object.keys(shown).length > 0;
  const detailsDirty = Object.keys(detailChanges).length > 0;
  const locked = !ready || saving;
  const canSave = !locked && dirty;
  const value = {
    enabled: details.enabled ?? d.enabled,
    trusted: details.trusted ?? d.trusted,
    blocked: details.blocked ?? d.blocked,
    hidpp: details.hidpp ?? d.hidpp?.enabled ?? false,
    profiles: details.profiles ?? d.profiles ?? [],
  };
  const stage = (update: DeviceChanges) => {
    setSaveError(null);
    onDetails((x) => pendingDetails({ ...x, ...update }, d));
  };
  const save = async () => {
    if (!canSave) return;
    setSaveError(null);
    const sent = shown;
    const at = tab;
    const result = await runSave({ type: "device.update", key: entry.key, ...sent });
    if (result.ok) onDetails((x) => settleDetails(x, sent));
    else setSaveError({ tab: at, message: result.message });
  };
  const discard = () => {
    setSaveError(null);
    onDetails((x) => Object.fromEntries(Object.entries(x).filter(([k]) => fieldTab(k) !== tab)));
  };
  const saveFailed = saveError?.tab === tab ? saveError.message : null;
  // Ctrl+S or Cmd+S saves while Details or Profiles is shown and no dialog is open; the Settings
  // tab has its own.
  const saveRef = useRef<(() => void) | null>(null);
  saveRef.current = tab === "details" || tab === "profiles" ? () => void save() : null;
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (!saveRef.current || e.key.toLowerCase() !== "s" || !(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return;
      if (document.querySelector("dialog[open]")) return;
      e.preventDefault();
      saveRef.current();
    };
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  }, []);
  const gamepadStatus = d.outputs.find((o) => o.role === "gamepad");
  // A failed list read is retried by reading the list again rather than asking the device.
  const listsFailed = !!entry.settingsError || !!entry.readError;
  const lastError = connectionError(d);
  const unloaded = entry.readError ? null : profileError(d);

  return (
    <Page
      icon={<DeviceIcon kind={entry.kind} size={20} />}
      active={connected}
      title={entry.name}
      status={
        <>
          <Pill tone={connected ? "ok" : d.blocked ? "warn" : "neutral"} dot>
            {deviceStatus(entry)}
          </Pill>
          {entry.battery && battery ? (
            <Pill tone={low ? "low" : "neutral"} dim={batteryStale(entry.battery)}>
              <BatteryGlyph percent={entry.battery.percent} charging={entry.battery.charging} low={low} />
              {battery}
            </Pill>
          ) : null}
        </>
      }
      nav={
        <TabBar
          id={tabs}
          label="Device"
          tabs={DEVICE_TABS.filter(([t]) => (t !== "settings" || settings) && (t !== "profiles" || layered))}
          value={tab}
          onChange={setTab}
        />
      }
      bar={
        tab === "settings" ? (
          <div ref={setSettingsBar} className="bar-slot" />
        ) : tab === "profiles" ? (
          <>
            {saveFailed ? <span className="bar-start error-text">{saveFailed}</span> : null}
            {saving ? <Spinner /> : null}
            <button disabled={saving || !dirty} onClick={discard}>
              <UndoIcon /> Discard
            </button>
            <button className="suggested" disabled={!canSave} onClick={() => void save()}>
              <CheckIcon /> Save
            </button>
          </>
        ) : tab === "diagnostics" ? (
          <>
            {failed("info") ? <span className="bar-start error-text">{failed("info")}</span> : null}
            {infoBusy ? <Spinner /> : null}
            <button
              disabled={infoBusy || !live}
              onClick={() => void perform("info", runInfo, { type: listsFailed || !connected ? "device.reload" : "device.refresh", key: entry.key })}
            >
              <RefreshIcon /> {listsFailed || failed("info") ? "Retry" : "Refresh"}
            </button>
          </>
        ) : (
          <>
            <span className="bar-start">
              <button
                className="destructive"
                disabled={!live}
                onClick={() => {
                  setFailure(null);
                  setForgetting(true);
                }}
              >
                <TrashIcon /> Forget Device
              </button>
              {saveFailed ? <span className="error-text">{saveFailed}</span> : failedText("connection")}
            </span>
            {busy || connectBusy || saving ? <Spinner /> : null}
            {connected || connecting ? (
              <button disabled={busy || !live} onClick={() => void perform("connection", run, { type: "device.disconnect", key: entry.key })}>
                <UnplugIcon /> Disconnect
              </button>
            ) : (
              <button
                className={canConnect && !detailsDirty ? "suggested" : undefined}
                disabled={busy || !canConnect}
                onClick={() => void perform("connection", runConnect, { type: "device.connect", key: entry.key })}
              >
                <PlugIcon /> Connect
              </button>
            )}
            <button disabled={saving || !dirty} onClick={discard}>
              <UndoIcon /> Discard
            </button>
            <button className="suggested" disabled={!canSave} onClick={() => void save()}>
              <CheckIcon /> Save
            </button>
          </>
        )
      }
    >
      {!entry.readError && d.inactive !== null && d.inactive !== "disabled" ? <Banner>{inactiveText(d)}</Banner> : null}

      <TabPanel id={tabs} value={tab}>
        {tab === "settings" ? (
          <Settings entry={entry} live={live} adapter={adapter} drafts={drafts} details={details} onDetails={onDetails} bar={settingsBar} />
        ) : tab === "profiles" ? (
          <DeviceLayers
            entry={entry}
            adapter={adapter}
            layers={value.profiles}
            changed={layerChanges !== undefined}
            locked={locked}
            onChange={(profiles) => stage({ profiles })}
          />
        ) : tab === "diagnostics" ? (
          <>
            {lastError ? (
              <Card title="Connection">
                <Facts>
                  <Fact label="Last Error">{errorText(lastError, "other")}</Fact>
                </Facts>
              </Card>
            ) : null}

            {unloaded ? (
              <Card title="Profiles">
                <Facts>
                  <Fact label="Status">
                    Not Loaded
                    <span className="fact-detail">{profileErrorText(unloaded)}</span>
                  </Fact>
                </Facts>
              </Card>
            ) : null}

            {entry.readError ? (
              <Card title="Saved Data">
                <Row
                  title={
                    errorText(entry.readError, "read") === READ_FAILED
                      ? "The adapter couldn't read this device's saved data. Try again."
                      : `The adapter couldn't read this device's saved data. ${errorText(entry.readError, "read")}`
                  }
                />
              </Card>
            ) : null}

            {entry.settingsError ? (
              <Card title="Device Settings">
                <Row
                  title={
                    entry.settingsError === READ_FAILED
                      ? "The adapter couldn't read the device's settings. Try again."
                      : `The adapter couldn't read the device's settings. ${entry.settingsError}`
                  }
                />
              </Card>
            ) : null}

            {d.hidpp ? (
              <Card title="Logitech Features">
                <Facts>
                  <Fact label="HID++ Protocol">{versionText(d.hidpp)}</Fact>
                  <Fact label="Status">{integrationText(d.hidpp)}</Fact>
                </Facts>
              </Card>
            ) : null}

            {gamepadStatus && gamepadStatus.state !== "disabled" ? (
              <Card title="Gamepad Output">
                <Facts>
                  <Fact label="Status">{stateText(gamepadStatus.state && OUTPUT_STATES[gamepadStatus.state], gamepadStatus.error)}</Fact>
                  {gamepadStatus.slot !== null ? <Fact label="Slot">{gamepadStatus.slot + 1}</Fact> : null}
                </Facts>
              </Card>
            ) : null}

            {connected && d.security ? (
              <Card title="Security">
                <Facts>
                  {securityFacts(d.security).map(([name, value]) => (
                    <Fact key={name} label={name}>
                      {value}
                    </Fact>
                  ))}
                </Facts>
              </Card>
            ) : null}

            <Card title="Identifiers">
              <Facts>
                {identifiers.map(([label, value]) => (
                  <Fact key={label} label={label} dim={!connected}>
                    {value}
                  </Fact>
                ))}
                <Fact label="Device ID">{d.id}</Fact>
              </Facts>
            </Card>
          </>
        ) : (
          <>
            <Card title="Connection">
              <SwitchRow
                title="Use This Device"
                checked={value.enabled}
                disabled={locked || (!value.enabled && full({ enabled: true }))}
                end={changes.enabled !== undefined ? <Staged label="Use This Device" /> : null}
                onChange={(enabled) => stage({ enabled })}
              />
              <SwitchRow
                title="Automatic Connections"
                checked={value.trusted}
                disabled={locked}
                end={changes.trusted !== undefined ? <Staged label="Automatic Connections" /> : null}
                onChange={(trusted) => stage({ trusted })}
              />
              <SwitchRow
                title="Logitech Features"
                checked={value.hidpp}
                disabled={locked || settingsBusy(entry)}
                end={changes.hidpp !== undefined ? <Staged label="Logitech Features" /> : null}
                onChange={(hidpp) => stage({ hidpp })}
              />
              <SwitchRow
                title="Block Connections"
                checked={value.blocked}
                disabled={locked || (value.blocked && full({ blocked: false }))}
                end={changes.blocked !== undefined ? <Staged label="Block Connections" /> : null}
                onChange={(blocked) => stage({ blocked })}
              />
            </Card>

            <Card title="Information">
              <Facts>
                {d.kinds.length ? <Fact label="Device Type">{kindText(entry.kind)}</Fact> : null}
                {info.map(([label, value]) => (
                  <Fact key={label} label={label} dim={!connected}>
                    {value}
                  </Fact>
                ))}
                {adapter ? <Fact label="Adapter">{adapter.name}</Fact> : null}
                {d.transport ? <Fact label="Bluetooth">{TRANSPORTS[d.transport]}</Fact> : null}
                {d.roles.length ? <Fact label="Input">{d.roles.map((r) => ROLES[r]).join(", ")}</Fact> : null}
              </Facts>
            </Card>
          </>
        )}
      </TabPanel>

      <Dialog open={forgetting} title={`Forget “${entry.name}”?`} onClose={() => setForgetting(false)}>
        <p className="dialog-body">Forgetting this device deletes its pairing and saved settings from the adapter.</p>
        {failed("unpair") ? <p className="dialog-body error-text">{failed("unpair")}</p> : null}
        <footer className="dialog-footer">
          <button onClick={() => setForgetting(false)}>
            <CloseIcon /> Cancel
          </button>
          <button
            className="destructive"
            disabled={busy}
            onClick={async () => {
              const result = await perform("unpair", run, { type: "device.unpair", key: entry.key });
              if (result.ok) setForgetting(false);
            }}
          >
            <TrashIcon /> Forget
          </button>
        </footer>
      </Dialog>
    </Page>
  );
}
