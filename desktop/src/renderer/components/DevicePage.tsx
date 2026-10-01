import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { isLow } from "../../shared/battery.ts";
import { enabledFull } from "../../shared/capacity.ts";
import { settingsBusy, settingsCurrent } from "../../shared/settings.ts";
import type { AdapterEntry, AppState, DeviceEntry, InfoEntry, Scalar, Setting, SettingsChange, SettingsSaveItem } from "../../shared/state.ts";
import {
  INACTIVE,
  INFO_LABELS,
  ROLES,
  SETTING_STATES,
  TRANSPORTS,
  WARNINGS_READ_FAILED,
  batteryStale,
  batteryText,
  choiceText,
  codeText,
  deviceStatus,
  infoValue,
  integrationText,
  kindText,
  securityFacts,
  settingInfo,
  settingOrder,
  versionText,
  warningFact,
  wheelFigures,
} from "../../shared/text.ts";
import { useAction } from "../api.ts";
import { Banner, Card, Dialog, Fact, Facts, Page, Pill, Row, Segmented, Spinner, Switch, SwitchRow, TabBar, TabPanel } from "./common.tsx";
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

/** The label of a setting this page shows; only known keys are listed. */
const label = (key: string) => settingInfo(key)!.label;

function valueText(key: string, v: Scalar | null): string {
  if (v === null) return "Unknown";
  if (typeof v === "boolean") return v ? "On" : "Off";
  if (typeof v === "string") return choiceText(key, v);
  return String(v);
}

const choiceLabel = (s: Setting, c: Scalar) => (typeof c === "string" ? choiceText(s.key, c) : String(c));

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
/** Text and color settings show their value without an editor. */
const editable = (s: Setting) => s.type === "bool" || s.type === "integer" || s.type === "enum";

/** Whether the setting takes `v`. */
function accepts(s: Setting, v: Scalar | null): v is Scalar {
  if (v === null) return false;
  if (s.type === "bool") return typeof v === "boolean";
  if (s.choices.length) return s.choices.includes(v);
  if (s.type !== "integer") return typeof v === "string";
  return typeof v === "number" && Number.isSafeInteger(v) && (s.min == null || v >= s.min) && (s.max == null || v <= s.max)
    && (v - (s.min ?? 0)) % (s.step ?? 1) === 0;
}

/** The range typed text must be in: a SmartShift threshold is 1-254, since
 * only its toggle turns it Off with 255. */
const typedRange = (s: Setting): Setting => (s.key === "wheel.threshold" ? { ...s, max: Math.min(s.max ?? 254, 254) } : s);

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
  if (draft.type === "forget") return { change: saved(s) ? { type: "forget", setting: s.key } : null, invalid: false };
  const value = draftValue(s, draft.value);
  if (value === null) return { change: null, invalid: true };
  if (!draft.policy && value === baseValue(s)) return { change: null, invalid: false };
  return { change: { type: "set", setting: s.key, value }, invalid: false };
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

/** One marker menu entry; the menu stages it without sending anything. */
type MarkerOption = [string, (() => void) | null];

/** The fixed-width state marker; writable rows open their staging menu from it. */
function Marker({ label, state, shape, options, sending, disabled }: {
  label: string;
  state: string;
  shape: MarkShape;
  options: MarkerOption[];
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
        <MarkerMenu
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

function MarkerMenu({ label, x, y, options, onClose }: {
  label: string;
  x: number;
  y: number;
  options: MarkerOption[];
  onClose: (refocus: boolean) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const menu = ref.current!;
    menu.showPopover();
    menu.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
    const outside = (e: PointerEvent) => !menu.contains(e.target as Node) && !(e.target as Element).closest?.("[aria-expanded=true]") && onClose(false);
    document.addEventListener("pointerdown", outside);
    const blur = () => onClose(false);
    window.addEventListener("blur", blur, { once: true });
    return () => {
      document.removeEventListener("pointerdown", outside);
      window.removeEventListener("blur", blur);
      menu.hidePopover();
    };
  }, []);
  const move = (step: number) => {
    const items = [...ref.current!.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    items[(at + step + items.length) % items.length]?.focus();
  };
  return (
    <div
      ref={ref}
      popover="manual"
      role="menu"
      aria-label={label}
      className="context-menu"
      style={{ left: Math.max(8, Math.min(x - 200, window.innerWidth - 208)), top: Math.min(y + 4, window.innerHeight - 40 * options.length - 16) }}
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose(true);
        else if (e.key === "ArrowDown") move(1);
        else if (e.key === "ArrowUp") move(-1);
        else if (e.key === "Tab") onClose(false);
        else return;
        e.preventDefault();
        e.stopPropagation();
      }}
    >
      {options.map(([text, run]) => (
        <button
          key={text}
          type="button"
          role="menuitem"
          disabled={!run}
          onClick={() => {
            onClose(true);
            run?.();
          }}
        >
          {text}
        </button>
      ))}
    </div>
  );
}

/** Stages `v` from a value control. Choosing or typing what the device keeps
 * anyway drops the draft, except that a staged save of an unsaved value stays one. */
function edit(s: Setting, drafts: Drafts, v: Scalar) {
  const current = drafts.get(s.key);
  const policy = !saved(s) && current?.type === "set" && !!current.policy;
  const next = draftValue(s, v);
  if (!policy && next !== null && next === baseValue(s)) drafts.set(s.key, undefined);
  else drafts.set(s.key, { type: "set", value: v, ...(policy ? { policy } : {}) });
}

/** The row's value control, showing the draft, else the saved value, else the reading. */
function Control({ s, draft, guards, drafts, invalid }: { s: Setting; draft: Draft | undefined; guards: FormGuards; drafts: Drafts; invalid: boolean }) {
  const name = label(s.key);
  const value: Scalar | null = draft?.type === "set" ? draft.value : draft?.type === "forget" ? s.value : baseValue(s);
  const change = (v: Scalar) => edit(s, drafts, v);
  if (!editable(s) || (s.type === "enum" && !s.choices.length)) return <span className="value">{valueText(s.key, s.value)}</span>;
  if (s.type === "bool")
    return <Switch label={name} checked={typeof value === "boolean" ? value : null} disabled={guards.locked} onChange={change} />;
  if (s.key === "wheel.threshold" && freeform(s)) return <SmartShift s={s} value={value} guards={guards} drafts={drafts} invalid={invalid} />;
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
            {valueText(s.key, value)}
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
      onRevert={draft ? () => drafts.set(s.key, undefined) : null}
    />
  );
}

/** SmartShift: On with a threshold of 1-254, or Off, which the device takes as 255. */
function SmartShift({ s, value, guards, drafts, invalid }: { s: Setting; value: Scalar | null; guards: FormGuards; drafts: Drafts; invalid: boolean }) {
  // Text typed into the threshold keeps it On, even while empty or invalid.
  const on = typeof value === "string" ? true : typeof value === "number" ? value !== 255 : null;
  const threshold = [s.saved, s.value].find((v): v is number => typeof v === "number" && v >= 1 && v <= 254) ?? 254;
  return (
    <>
      <Switch label={label(s.key)} checked={on} disabled={guards.locked} onChange={(v) => edit(s, drafts, v ? threshold : 255)} />
      {on ? (
        <IntegerControl
          s={typedRange(s)}
          text={String(value)}
          invalid={invalid}
          disabled={guards.locked}
          label={`${label(s.key)} Threshold`}
          onEdit={(v) => edit(s, drafts, v)}
          onRevert={drafts.get(s.key) ? () => drafts.set(s.key, undefined) : null}
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
  const info = settingInfo(s.key)!;
  const draft = drafts.get(s.key);
  const { change, invalid } = pendingChange(s, draft);
  const staged = change !== null || invalid;
  const settable = fresh && accepts(s, s.value);
  const forget: MarkerOption = ["Forget Saved Value", () => drafts.set(s.key, { type: "forget" })];
  const keep = (text: string): MarkerOption => [text, settable ? () => drafts.set(s.key, { type: "set", value: s.value!, policy: true }) : null];
  let shape: MarkShape;
  let state: string;
  let options: MarkerOption[];
  if (staged) {
    [shape, state, options] = ["draft", "Changed", [["Undo Change", () => drafts.set(s.key, undefined)]]];
  } else if (!saved(s)) {
    [shape, state, options] = ["outline", SETTING_STATES.unmanaged, [keep("Save Current Value")]];
  } else if (s.state === "changed_on_device") {
    [shape, state, options] = ["differs", SETTING_STATES.changed_on_device, [keep("Save Device Value"), forget]];
  } else if (s.error || s.state === "unsupported") {
    [shape, state, options] = ["problem", SETTING_STATES[s.error ? "error" : "unsupported"], [forget]];
  } else {
    [shape, state, options] = ["filled", SETTING_STATES[s.state ?? "pending"], [forget]];
  }

  const notes: ReactNode[] = [];
  if (item?.status === "not_saved") notes.push(<span key="save" className="error-text">Couldn't Save{item.error ? `: ${item.error}` : ""}</span>);
  else if (item?.status === "not_sent") notes.push(<span key="save">Not Sent{item.error ? `: ${item.error}` : ""}</span>);
  if (invalid) notes.push(<span key="range" className="error-text">{rangeText(typedRange(s))}</span>);
  if (saved(s) && s.state === "changed_on_device" && fresh) notes.push(<span key="device">Device: {valueText(s.key, s.value)}</span>);
  if (s.error) notes.push(<span key="error">{codeText(s.error)}</span>);

  const labelId = `setting-${s.key}`;
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
        {info.unit ? <span className="unit">{info.unit}</span> : null}
      </fieldset>
      <div className="setting-marker">
        <Marker label={info.label} state={state} shape={shape} options={options} sending={item?.status === "saving"} disabled={guards.busy} />
      </div>
    </div>
  );
}

/** A value the device only reports, shown with the settings it belongs to. */
function ReadingRow({ f, dim }: { f: InfoEntry; dim: boolean }) {
  const info = settingInfo(f.key)!;
  return (
    <div className={dim ? "setting-row stale" : "setting-row"} role="group" aria-label={info.label}>
      <div className="setting-label">
        <div className="row-title">{info.label}</div>
      </div>
      <div className="setting-control">
        <span className="value">{typeof f.value === "string" ? choiceText(f.key, f.value) : valueText(f.key, f.value)}</span>
        {info.unit ? <span className="unit">{info.unit}</span> : null}
      </div>
      <div className="setting-marker" />
    </div>
  );
}

/** The settings tab's contents: settings the page knows, readings shown with them, and the wheel's figures. */
function settingsView(entry: DeviceEntry) {
  const rows = (entry.settings ?? []).filter((s) => settingInfo(s.key)).sort((a, b) => settingOrder(a.key) - settingOrder(b.key));
  const keys = new Set(rows.map((s) => s.key));
  const readings = entry.device.info.filter((f) => settingInfo(f.key) && !keys.has(f.key)).sort((a, b) => settingOrder(a.key) - settingOrder(b.key));
  const figures = wheelFigures(entry.device.info);
  return { rows, readings, figures };
}

/** Whether the device page has a settings tab; saved settings stay visible while disconnected. */
function hasSettings(entry: DeviceEntry) {
  const { rows, readings, figures } = settingsView(entry);
  return rows.length > 0 || readings.length > 0 || figures.length > 0 || entry.settingsError !== null || starting(entry);
}

/** HID++ is coming up on the connected device, reading its settings. */
const starting = (entry: DeviceEntry) => entry.device.state === "connected" && entry.device.hidpp?.state === "starting";

/** The settings form: every staged change goes to the device together with
 * Save. Its buttons go in the page bar, `bar`, outside the form. */
function Settings({ entry, adapter, drafts, bar }: {
  entry: DeviceEntry;
  adapter: AdapterEntry | undefined;
  drafts: Drafts;
  bar: HTMLElement | null;
}) {
  const formId = useId();
  const [refreshing, runRefresh] = useAction(true);
  const [saving, runSave] = useAction(true);
  const [reloading, runReload] = useAction(true);
  const [problem, setProblem] = useState<string | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const d = entry.device;
  const { rows: settings, readings, figures } = settingsView(entry);
  const categories = [...new Set([...settings.map((s) => s.key), ...readings.map((f) => f.key)].sort((a, b) => settingOrder(a) - settingOrder(b)).map((k) => settingInfo(k)!.category))];
  if (figures.length && !categories.includes("Wheel")) categories.push("Wheel");
  const connected = d.state === "connected";
  const reachable = adapter?.connection === "connected";
  const busy = !reachable || saving || refreshing || settingsBusy(entry);
  const guards: FormGuards = { locked: busy, busy };
  const submission = entry.settingsSave;
  const items = submission?.items ?? [];
  const itemFor = (key: string) => items.find((i) => i.change.setting === key);
  const staged = settings.map((s) => ({ s, draft: drafts.get(s.key), ...pendingChange(s, drafts.get(s.key)) }));
  const changes = staged.filter((r) => r.change !== null);
  const dirty = staged.some((r) => r.change !== null || r.invalid);
  const invalid = staged.some((r) => r.invalid);
  const canSave = !busy && changes.length > 0 && !invalid;
  // Retry saves a failed setting's value again, which applies it again; a
  // draft of that setting stands in its way.
  const retryable = staged.filter((r) => r.s.error && saved(r.s));
  const retry = retryable.filter((r) => !r.change && !r.invalid);
  const canRetry = !busy && retry.length > 0;

  const submit = async (list: SettingsChange[], cohort: [string, Draft | undefined][]) => {
    setProblem(null);
    const result = await runSave({ type: "settings.save", key: entry.key, changes: list });
    for (const item of result.settingsSave?.items ?? []) {
      const draft = cohort.find(([key]) => key === item.change.setting)?.[1];
      if (draft && item.status === "saved") drafts.set(item.change.setting, undefined, draft);
    }
    if (!result.ok && !result.inline) setProblem(result.message);
  };
  const save = () => {
    if (!canSave) return;
    void submit(changes.map((r) => r.change!), changes.map((r) => [r.s.key, r.draft]));
  };
  const onSubmit = (e: FormEvent) => {
    e.preventDefault();
    save();
  };

  // Ctrl+S or Cmd+S saves while the settings form is shown and no dialog is open.
  const saveRef = useRef(save);
  saveRef.current = save;
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
      <span className="muted">Couldn't read the device's settings.</span>
      <button type="button" disabled={reloading} onClick={() => void runReload({ type: "device.reload", key: entry.key })}>
        <RefreshIcon /> Retry
      </button>
    </>
  );
  const hidppError = d.hidpp?.error ?? null;

  if (!settings.length && !readings.length && !figures.length)
    return entry.settingsError ? (
      <div className="panel-state">{loadProblem}</div>
    ) : hidppError ? (
      <p className="muted">Settings unavailable: {codeText(hidppError)}.</p>
    ) : (
      <div className="panel-state">
        <Spinner />
        <span className="muted">Reading the device's settings…</span>
      </div>
    );

  const counts: [string, number][] = [
    ["Couldn't Save", items.filter((i) => i.status === "not_saved").length],
    ["Not Sent", items.filter((i) => i.status === "not_sent").length],
  ];
  const failures = submission && !submission.running ? counts.filter(([, n]) => n > 0).map(([t, n]) => `${t} ${n}`).join(" · ") : "";
  const note = problem ?? (failures || (hidppError ? codeText(hidppError) : null));
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
      <button type="button" {...guard(!busy && connected, () => void refresh())}>
        <RefreshIcon /> Refresh
      </button>
      {retryable.length ? (
        <button type="button" {...guard(canRetry, () => void submit(retry.map((r) => ({ type: "set", setting: r.s.key, value: r.s.saved! })), []))}>
          <RefreshIcon /> Retry
        </button>
      ) : null}
      <button type="button" {...guard(!busy && dirty, () => drafts.clear())}>
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
            save();
          }
        }}
      >
        <div className="settings-fields">
          {categories.map((category) => (
            <Card key={category} title={category}>
              {settings
                .filter((s) => settingInfo(s.key)!.category === category)
                .map((s) => (
                  <SettingRow key={s.key} entry={entry} s={s} drafts={drafts} guards={guards} item={itemFor(s.key)} />
                ))}
              {readings
                .filter((f) => settingInfo(f.key)!.category === category)
                .map((f) => (
                  <ReadingRow key={f.key} f={f} dim={!connected} />
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

export function DevicePage({ state, entry, drafts }: { state: AppState; entry: DeviceEntry; drafts: Drafts }) {
  const [busy, run] = useAction(true);
  const [connectBusy, runConnect] = useAction(true);
  const [infoBusy, runInfo] = useAction(true);
  // The last failed action on this page, shown next to the control that ran it.
  const [failure, setFailure] = useState<{ at: string; message: string } | null>(null);
  const [forgetting, setForgetting] = useState(false);
  // A device opens on its details, where Connect is; the tab stays put as the connection changes.
  const [chosen, setTab] = useState<"details" | "settings">("details");
  const tabs = useId();
  const [settingsBar, setSettingsBar] = useState<HTMLElement | null>(null);
  const d = entry.device;
  const adapter = state.adapters.find((a) => a.id === entry.adapterId);
  const connected = d.state === "connected";
  const settings = hasSettings(entry);
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
  const tab = settings ? chosen : "details";
  const low = isLow(entry.battery, state.preferences.lowBatteryPercent);
  const battery = batteryText(entry.battery);
  const canConnect = d.inactive === null && d.state === "disconnected";
  const peers = state.devices.filter((x) => x.adapterId === entry.adapterId).map((x) => x.device);
  // Turning the device on is not offered while every place for its transport is in use.
  const full = !d.enabled && enabledFull(adapter?.status ?? null, peers, d);
  const info = Object.keys(INFO_LABELS).flatMap((key) => d.info.filter((f) => f.key === key));
  const set = (type: "device.enabled" | "device.trusted" | "device.blocked" | "device.hidpp") => (value: boolean) =>
    void perform(type, run, { type, key: entry.key, value });
  const connectionProblem = failed("connection") ?? (d.state === "disconnected" && d.error ? codeText(d.error) : null);
  const warningsFailed = !!entry.warningsError;

  return (
    <Page
      icon={<DeviceIcon kind={entry.kind} size={20} />}
      active={connected}
      title={entry.name}
      status={
        <>
          <Pill tone={connected ? "ok" : d.blocked ? "warn" : "neutral"} dot>
            {deviceStatus(d)}
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
        settings ? (
          <TabBar
            id={tabs}
            label="Device"
            tabs={[
              ["details", "Details"],
              ["settings", "Settings"],
            ]}
            value={tab}
            onChange={setTab}
          />
        ) : undefined
      }
      bar={
        tab === "settings" ? (
          <div ref={setSettingsBar} className="bar-slot" />
        ) : (
          <>
            <span className="bar-start">
              <button
                className="destructive"
                onClick={() => {
                  setFailure(null);
                  setForgetting(true);
                }}
              >
                <TrashIcon /> Forget Device
              </button>
            </span>
            {busy || connectBusy ? <Spinner /> : null}
            {connected || connecting ? (
              <button disabled={busy} onClick={() => void perform("connection", run, { type: "device.disconnect", key: entry.key })}>
                <UnplugIcon /> Disconnect
              </button>
            ) : (
              <button
                className={canConnect ? "suggested" : undefined}
                disabled={busy || !canConnect}
                onClick={() => void perform("connection", runConnect, { type: "device.connect", key: entry.key })}
              >
                <PlugIcon /> Connect
              </button>
            )}
          </>
        )
      }
    >
      {connectionProblem ? <Banner kind="error">{connectionProblem}</Banner> : null}
      {d.inactive !== null && d.inactive !== "disabled" ? <Banner>Inactive: {INACTIVE[d.inactive]}</Banner> : null}

      <TabPanel id={tabs} value={settings ? tab : null}>
        {tab === "settings" ? (
          <Settings entry={entry} adapter={adapter} drafts={drafts} bar={settingsBar} />
        ) : (
          <>
            <Card title="Connection">
              <SwitchRow
                title="Use This Device"
                subtitle={failedText("device.enabled")}
                checked={d.enabled}
                disabled={busy || full}
                onChange={set("device.enabled")}
              />
              <SwitchRow
                title="Automatic Connections"
                subtitle={failedText("device.trusted")}
                checked={d.trusted}
                disabled={busy}
                onChange={set("device.trusted")}
              />
              <SwitchRow
                title="Logitech Features"
                subtitle={failedText("device.hidpp")}
                checked={d.hidpp?.enabled ?? false}
                disabled={busy || settingsBusy(entry)}
                onChange={set("device.hidpp")}
              />
              <SwitchRow
                title="Block Connections"
                subtitle={failedText("device.blocked")}
                checked={d.blocked}
                disabled={busy}
                onChange={set("device.blocked")}
              />
            </Card>

            <Card title="Information">
              <Facts>
                {warningsFailed ? <Fact label="Device Warnings">{WARNINGS_READ_FAILED}</Fact> : null}
                {(entry.warnings ?? []).map((w, i) => {
                  const fact = warningFact(w);
                  return (
                    <Fact key={i} label={fact.label} dim={!connected}>
                      {fact.text}
                      <span className="fact-detail">{fact.context}</span>
                    </Fact>
                  );
                })}
                {d.hidpp ? <Fact label="HID++ Protocol">{versionText(d.hidpp)}</Fact> : null}
                {d.hidpp ? <Fact label="Logitech Features">{integrationText(d.hidpp)}</Fact> : null}
                {d.kind !== "unknown" ? <Fact label="Device Type">{kindText(entry.kind)}</Fact> : null}
                {info.map((f) => (
                  <Fact key={f.key} label={INFO_LABELS[f.key]} dim={!connected}>
                    {infoValue(f)}
                  </Fact>
                ))}
                {adapter ? <Fact label="Adapter">{adapter.name}</Fact> : null}
                {d.transport ? <Fact label="Bluetooth">{TRANSPORTS[d.transport]}</Fact> : null}
                {d.roles.length ? <Fact label="Input">{d.roles.map((r) => ROLES[r]).join(", ")}</Fact> : null}
                {connected && d.security
                  ? securityFacts(d.security).map(([name, value]) => (
                      <Fact key={name} label={name}>
                        {value}
                      </Fact>
                    ))
                  : null}
                <Fact label="Device ID">{d.id}</Fact>
              </Facts>
              {failed("info") ? (
                <Row title="Couldn't Read Information" subtitle={failedText("info")} />
              ) : entry.warnings === null && !warningsFailed ? (
                <Row title="Reading Information…">
                  <Spinner />
                </Row>
              ) : null}
              {connected || warningsFailed ? (
                <div className="card-actions">
                  {infoBusy ? <Spinner /> : null}
                  <button
                    disabled={infoBusy}
                    onClick={() => void perform("info", runInfo, { type: warningsFailed || !connected ? "device.reload" : "device.refresh", key: entry.key })}
                  >
                    <RefreshIcon /> {warningsFailed || failed("info") ? "Retry" : "Refresh"}
                  </button>
                </div>
              ) : null}
            </Card>
          </>
        )}
      </TabPanel>

      <Dialog open={forgetting} title={`Forget “${entry.name}”?`} onClose={() => setForgetting(false)}>
        <p className="dialog-body">The adapter deletes its pairing and saved settings for this device.</p>
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
