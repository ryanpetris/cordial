import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import type { Setting, SettingKey, SettingValue } from "../../protocol/types.ts";
import { isLow } from "../../shared/battery.ts";
import { settingsBusy, settingsLive } from "../../shared/settings.ts";
import type { AdapterEntry, AppState, DeviceEntry, SettingsChange, SettingsSaveItem } from "../../shared/state.ts";
import {
  DISABLED,
  NORMALIZATION,
  ROLES,
  SETTINGS,
  SETTING_STATES,
  TRANSPORTS,
  VALIDATION,
  WARNINGS,
  batteryStale,
  batteryText,
  choiceText,
  codeText,
  deviceStatus,
  infoLabel,
  infoValue,
  securityFacts,
  settingOrder,
  wheelInfo,
} from "../../shared/text.ts";
import { act, useAction } from "../api.ts";
import { Banner, Card, Dialog, Fact, Facts, Page, Pill, Row, Segmented, Spinner, Switch, SwitchRow, Tabs } from "./common.tsx";
import { BatteryGlyph, DeviceIcon, RefreshIcon, StateMark, type MarkShape } from "./icons.tsx";

function valueText(s: Setting, v: SettingValue): string {
  if (v === null) return "Unknown";
  if (typeof v === "boolean") return v ? "On" : "Off";
  if (typeof v === "string") return choiceText(s.key, v);
  return String(v);
}

const choiceLabel = (s: Setting, c: SettingValue) => (typeof c === "string" ? choiceText(s.key, c) : String(c));

/** Whether a setting's choices fit side by side as joined buttons. */
const short = (s: Setting) => s.choices.length <= 3 && s.choices.reduce<number>((n, c) => n + choiceLabel(s, c).length, 0) <= 24;

/** A staged change. A `policy` save comes from Save Current Value or Save
 * Device Value and is a change even when it matches the shown value. */
export type Draft = { type: "set"; value: boolean | number | string; policy?: boolean } | { type: "forget" };

/** A device's staged changes. `set` with `expected` changes only a draft still equal to it. */
export interface Drafts {
  get(setting: SettingKey): Draft | undefined;
  set(setting: SettingKey, draft: Draft | undefined, expected?: Draft): void;
  clear(): void;
}

const freeform = (s: Setting) => s.type === "integer" && !s.choices.length;

/** Whether the setting's setter takes `v`. */
function accepts(s: Setting, v: SettingValue): v is boolean | number | string {
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

/** The value a set draft saves, or null when it isn't one the setter takes. */
function draftValue(s: Setting, value: boolean | number | string): boolean | number | string | null {
  if (!freeform(s)) return accepts(s, value) ? value : null;
  const n = Number(value);
  return String(value).trim() !== "" && accepts(typeof value === "string" ? typedRange(s) : s, n) ? n : null;
}

/** The value the device keeps when nothing is staged: the saved value, else the reading. */
const baseValue = (s: Setting) => (s.managed ? s.desired : s.observed);

/** A draft's change to submit; null when it matches what is saved or it is invalid. */
function pendingChange(s: Setting, draft: Draft | undefined): { change: SettingsChange | null; invalid: boolean } {
  if (!draft) return { change: null, invalid: false };
  if (draft.type === "forget") return { change: s.managed ? { type: "forget", setting: s.key } : null, invalid: false };
  const value = draftValue(s, draft.value);
  if (value === null) return { change: null, invalid: true };
  if (!draft.policy && value === baseValue(s)) return { change: null, invalid: false };
  return { change: { type: "set", setting: s.key, value }, invalid: false };
}

/** Whether a row shows a current reading: a row can stay marked fresh after
 * its list was invalidated or the device stopped reporting. */
const settingFresh = (entry: DeviceEntry, s: Setting) =>
  s.fresh && !!entry.settings?.current && entry.device.state === "connected" && entry.device.normalization_state !== "resetting";

/** The form's shared guards: whether values can be edited, and whether staging and the footer can act. */
interface FormGuards {
  /** Value controls are disabled. */
  locked: boolean;
  /** Nothing can be staged, saved or refreshed. */
  busy: boolean;
  live: boolean;
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
function edit(s: Setting, drafts: Drafts, v: boolean | number | string) {
  const current = drafts.get(s.key);
  const policy = !s.managed && current?.type === "set" && !!current.policy;
  const next = draftValue(s, v);
  if (!policy && next !== null && next === baseValue(s)) drafts.set(s.key, undefined);
  else drafts.set(s.key, { type: "set", value: v, ...(policy ? { policy } : {}) });
}

/** The row's value control, showing the draft, else the saved value, else the reading. */
function Control({ s, draft, guards, drafts, invalid }: { s: Setting; draft: Draft | undefined; guards: FormGuards; drafts: Drafts; invalid: boolean }) {
  const label = SETTINGS[s.key].label;
  const value: SettingValue = draft?.type === "set" ? draft.value : draft?.type === "forget" ? s.observed : baseValue(s);
  const change = (v: boolean | number | string) => edit(s, drafts, v);
  if (!s.writable || (s.type === "text" && !s.choices.length)) return <span className="value">{valueText(s, s.observed)}</span>;
  if (s.type === "bool")
    return <Switch label={label} checked={typeof value === "boolean" ? value : null} disabled={guards.locked} onChange={change} />;
  if (s.key === "wheel.threshold" && freeform(s)) return <SmartShift s={s} value={value} guards={guards} drafts={drafts} invalid={invalid} />;
  if (s.choices.length && value !== null && s.choices.includes(value) && short(s))
    return (
      <Segmented
        label={label}
        options={s.choices.filter((c) => c !== null).map((c) => [c, choiceLabel(s, c)])}
        value={value}
        disabled={guards.locked}
        onChange={change}
      />
    );
  if (s.choices.length)
    return (
      <select
        aria-label={label}
        value={value === null ? "" : String(value)}
        disabled={guards.locked}
        onChange={(e) => {
          const choice = s.choices.find((c) => String(c) === e.target.value);
          if (choice !== undefined && choice !== null) change(choice);
        }}
      >
        {value === null || !s.choices.includes(value) ? (
          <option value={value === null ? "" : String(value)} disabled>
            {valueText(s, value)}
          </option>
        ) : null}
        {s.choices.map((c) => (
          <option key={String(c)} value={String(c)}>
            {choiceLabel(s, c)}
          </option>
        ))}
      </select>
    );
  if (s.type === "integer")
    return (
      <IntegerControl
        s={s}
        text={typeof value === "string" || typeof value === "number" ? String(value) : ""}
        invalid={invalid}
        disabled={guards.locked}
        label={label}
        onEdit={change}
        onRevert={draft ? () => drafts.set(s.key, undefined) : null}
      />
    );
  return <span className="value">{valueText(s, s.observed)}</span>;
}

/** SmartShift: On with a threshold of 1-254, or Off, which the device takes as 255. */
function SmartShift({ s, value, guards, drafts, invalid }: { s: Setting; value: SettingValue; guards: FormGuards; drafts: Drafts; invalid: boolean }) {
  // Text typed into the threshold keeps it On, even while empty or invalid.
  const on = typeof value === "string" ? true : typeof value === "number" ? value !== 255 : null;
  const threshold = [s.desired, s.observed].find((v): v is number => typeof v === "number" && v >= 1 && v <= 254) ?? 254;
  return (
    <>
      <Switch label={SETTINGS[s.key].label} checked={on} disabled={guards.locked} onChange={(v) => edit(s, drafts, v ? threshold : 255)} />
      {on ? (
        <IntegerControl
          s={typedRange(s)}
          text={String(value)}
          invalid={invalid}
          disabled={guards.locked}
          label={`${SETTINGS[s.key].label} Threshold`}
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

/** One HID++ setting: label, value control, unit and state marker. */
function SettingRow({ entry, s, drafts, guards, item }: {
  entry: DeviceEntry;
  s: Setting;
  drafts: Drafts;
  guards: FormGuards;
  /** This setting's outcome in the latest submission. */
  item: SettingsSaveItem | undefined;
}) {
  const fresh = settingFresh(entry, s);
  const info = SETTINGS[s.key];
  const draft = drafts.get(s.key);
  const { change, invalid } = pendingChange(s, draft);
  const staged = change !== null || invalid;
  const settable = fresh && accepts(s, s.observed);
  const forget: MarkerOption = ["Forget Saved Value", () => drafts.set(s.key, { type: "forget" })];
  const keep = (text: string): MarkerOption => [text, settable ? () => drafts.set(s.key, { type: "set", value: s.observed as boolean | number | string, policy: true }) : null];
  let shape: MarkShape;
  let state: string;
  let options: MarkerOption[];
  if (staged) {
    [shape, state, options] = ["draft", "Changed", [["Undo Change", () => drafts.set(s.key, undefined)]]];
  } else if (!s.managed) {
    [shape, state, options] = ["outline", SETTING_STATES.unmanaged, [keep("Save Current Value")]];
  } else if (s.state === "changed_on_device") {
    [shape, state, options] = ["differs", SETTING_STATES.changed_on_device, [keep("Save Device Value"), forget]];
  } else if (s.state === "error" || s.state === "uncertain" || s.state === "unsupported") {
    [shape, state, options] = ["problem", SETTING_STATES[s.state], [forget]];
  } else {
    [shape, state, options] = ["filled", SETTING_STATES[s.state], [forget]];
  }

  const notes: ReactNode[] = [];
  if (item?.status === "not_saved") notes.push(<span key="save" className="error-text">Couldn't Save{item.error ? `: ${item.error}` : ""}</span>);
  else if (item?.status === "not_applied") notes.push(<span key="save" className="error-text">Didn't Apply{item.error ? `: ${item.error}` : ""}</span>);
  else if (item?.status === "not_sent") notes.push(<span key="save">Not Sent{item.error ? `: ${item.error}` : ""}</span>);
  if (invalid) notes.push(<span key="range" className="error-text">{rangeText(typedRange(s))}</span>);
  if (s.writable && s.managed && s.state === "changed_on_device" && fresh) notes.push(<span key="device">Device: {valueText(s, s.observed)}</span>);
  if (s.error && item?.status !== "not_applied") notes.push(<span key="error">{s.managed ? codeText(s.error) : `Read Failed: ${codeText(s.error)}`}</span>);

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
        {s.writable ? (
          <Marker label={info.label} state={state} shape={shape} options={options} sending={item?.status === "saving"} disabled={guards.busy} />
        ) : null}
      </div>
    </div>
  );
}

/** The read-only wheel capability readout as a strip of figures. */
function WheelInfo({ entry, s }: { entry: DeviceEntry; s: Setting }) {
  const fresh = settingFresh(entry, s);
  const facts = wheelInfo(s.observed, s.feature_version);
  if (!facts)
    return (
      <Row title={SETTINGS[s.key].label} dim={!fresh}>
        <span className="value">{s.observed === null ? "Unknown" : `Unrecognized (${String(s.observed)})`}</span>
      </Row>
    );
  return (
    <div className={fresh ? "figures" : "figures dim"}>
      {facts.map(([label, value]) => (
        <span key={label} className="figure">
          <span className="figure-value">{value}</span>
          <span className="figure-label">{label}</span>
        </span>
      ))}
    </div>
  );
}

/** Whether the settings tab has anything to show; saved settings stay visible while disconnected. */
function hasSettings(entry: DeviceEntry) {
  const d = entry.device;
  const view = entry.settings;
  if (view?.settings.length) return true;
  if (d.state !== "connected") return false;
  return !view || view.loadError !== null || d.settings_error !== null || reading(entry);
}

const reading = (entry: DeviceEntry) =>
  entry.device.settings_state === "discovering" || entry.device.settings_state === "pending" || !entry.settings?.current;

/** How long a background read may go unanswered before the page reports it. */
const GRACE_MS = 10_000;
/** How long the adapter's own settings discovery may run; its HID++ work can take this long. */
const DISCOVERY_MS = 90_000;

/** Whether `waiting` has held for `ms` since it began or `restart` changed; checked at each deadline. */
function useWaited(waiting: boolean, restart: number | string): (ms: number) => boolean {
  const [clock, setClock] = useState<{ since: number; restart: number | string } | null>(null);
  const [, tick] = useState(0);
  useEffect(() => {
    setClock(waiting ? { since: Date.now(), restart } : null);
    if (!waiting) return;
    const timers = [GRACE_MS, DISCOVERY_MS].map((ms) => setTimeout(() => tick((n) => n + 1), ms));
    return () => timers.forEach(clearTimeout);
  }, [waiting, restart]);
  return (ms) => waiting && clock !== null && clock.restart === restart && Date.now() - clock.since >= ms;
}

/** Whether the current catalog already shows a failed change took effect,
 * as after a reconnect or Refresh, so its failure is no longer news. */
function resolved(entry: DeviceEntry, s: Setting | undefined, item: SettingsSaveItem): boolean {
  if (!s || (item.status !== "not_applied" && item.status !== "not_saved")) return false;
  if (item.change.type === "forget") return !s.managed;
  return s.managed && s.desired === item.change.value && s.state === "applied" && settingFresh(entry, s);
}

/** Statuses that end a submitted change's draft: it is saved, even if not applied. */
const DONE: SettingsSaveItem["status"][] = ["applied", "saved", "not_applied"];

/** The settings form: every staged change goes to the device together with Save. */
function Settings({ entry, adapter, drafts, waited, onRetry }: {
  entry: DeviceEntry;
  adapter: AdapterEntry | undefined;
  drafts: Drafts;
  waited: (ms: number) => boolean;
  onRetry: () => void;
}) {
  const [refreshing, runRefresh] = useAction(true);
  const [saving, runSave] = useAction(true);
  const [problem, setProblem] = useState<string | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const d = entry.device;
  const view = entry.settings;
  const settings = [...(view?.settings ?? [])].sort((a, b) => settingOrder(a.key) - settingOrder(b.key));
  const categories = [...new Set(settings.map((s) => SETTINGS[s.key].category))];
  const live = settingsLive(entry);
  const reachable = adapter?.connection === "connected";
  const busy = !reachable || saving || refreshing || settingsBusy(entry);
  const guards: FormGuards = { locked: busy || !live, busy, live };
  const submission = entry.settingsSave ?? null;
  // Failures the catalog has since shown resolved are left out.
  const items = (submission?.items ?? []).filter((i) => !resolved(entry, settings.find((s) => s.key === i.change.setting), i));
  const itemFor = (key: SettingKey) => items.find((i) => i.change.setting === key);
  const staged = settings.map((s) => ({ s, draft: drafts.get(s.key), ...pendingChange(s, drafts.get(s.key)) }));
  const pending = staged.filter((r) => r.change !== null);
  const dirty = staged.some((r) => r.change !== null || r.invalid);
  // Offline, only forgetting saved values reaches the adapter; value changes stay staged.
  const changes = live ? pending : pending.filter((r) => r.change!.type === "forget");
  const invalid = live && staged.some((r) => r.invalid);
  const canSave = !busy && changes.length > 0 && !invalid;
  // Retry resends a value that was saved but didn't apply, while it is still
  // the saved one, the device can take it now and no draft of that setting
  // stands in its way; other drafts stay staged.
  const retryable = (submission && !submission.running ? items : []).filter((i) => i.status === "not_applied");
  const retry = retryable.filter((i) => {
    const s = settings.find((s) => s.key === i.change.setting);
    const row = staged.find((r) => r.s.key === i.change.setting);
    return !!s && i.change.type === "set" && s.writable && s.managed && s.state !== "unsupported" && s.desired === i.change.value
      && accepts(s, i.change.value) && !row?.change && !row?.invalid;
  });
  const canRetry = !busy && live && d.hidpp_enabled && retry.length > 0;
  // The adapter discovering a device's settings is progress, so it gets longer.
  const failing = waited(d.settings_state === "discovering" ? DISCOVERY_MS : GRACE_MS);

  const submit = async (list: SettingsChange[], cohort: [SettingKey, Draft | undefined][]) => {
    setProblem(null);
    const result = await runSave({ type: "settings.save", key: entry.key, changes: list });
    for (const item of result.settingsSave?.items ?? []) {
      const draft = cohort.find(([key]) => key === item.change.setting)?.[1];
      if (draft && DONE.includes(item.status)) drafts.set(item.change.setting, undefined, draft);
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
      <button type="button" onClick={onRetry}>
        Retry
      </button>
    </>
  );

  if (!settings.length)
    return d.settings_state === "unsupported" || d.settings_state === "error" ? (
      <p className="muted">Settings unavailable{d.settings_error ? `: ${codeText(d.settings_error)}.` : "."}</p>
    ) : failing ? (
      <div className="panel-state">{loadProblem}</div>
    ) : (
      <div className="panel-state">
        <Spinner />
        <span className="muted">Reading the device's settings…</span>
      </div>
    );

  const loadFailed = failing && !!view?.loadError;
  const counts: [string, number][] = [
    ["Couldn't Save", items.filter((i) => i.status === "not_saved").length],
    ["Didn't Apply", retryable.length],
    ["Not Sent", items.filter((i) => i.status === "not_sent").length],
  ];
  const failures = submission && !submission.running ? counts.filter(([, n]) => n > 0).map(([t, n]) => `${t} ${n}`).join(" · ") : "";
  const note = problem ?? (failures || (view?.result?.error ?? (d.settings_error ? codeText(d.settings_error) : null)));
  const refresh = async () => {
    setProblem(null);
    const result = await runRefresh({ type: "settings.refresh", key: entry.key });
    if (!result.ok) setProblem(result.message);
  };
  // Footer buttons stay focusable while unavailable, so a focused Save keeps focus while it runs.
  const guard = (enabled: boolean, run: () => void) => ({
    "aria-disabled": !enabled,
    onClick: () => {
      if (enabled) run();
    },
  });
  const working = saving || refreshing || !!submission?.running || d.settings_state === "applying" || d.settings_state === "discovering";

  return (
    <form
      ref={form}
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
        {categories.map((category) => {
          const rows = settings.filter((s) => SETTINGS[s.key].category === category);
          const wheel = rows.find((s) => s.key === "wheel.info");
          return (
            <Card key={category} title={category}>
              {rows
                .filter((s) => s !== wheel)
                .map((s) => (
                  <SettingRow key={s.key} entry={entry} s={s} drafts={drafts} guards={guards} item={itemFor(s.key)} />
                ))}
              {wheel ? <WheelInfo entry={entry} s={wheel} /> : null}
            </Card>
          );
        })}
      </div>
      <div className="settings-footer">
        <span className="actions-note">
          {working ? <Spinner /> : null}
          {note ? <span className="error-text">{note}</span> : null}
          {loadFailed ? loadProblem : null}
        </span>
        <button type="button" {...guard(!busy && live && !loadFailed, () => void refresh())}>
          <RefreshIcon /> Refresh
        </button>
        {retryable.length ? (
          <button type="button" {...guard(canRetry, () => void submit(retry.map((i) => i.change), []))}>
            Retry
          </button>
        ) : null}
        <button type="button" {...guard(!busy && dirty, () => drafts.clear())}>
          Discard
        </button>
        <button type="submit" className="suggested" aria-disabled={!canSave}>
          Save
        </button>
      </div>
    </form>
  );
}

export function DevicePage({ state, entry, drafts, onAdd }: { state: AppState; entry: DeviceEntry; drafts: Drafts; onAdd: () => void }) {
  const [busy, run] = useAction(true);
  const [connectBusy, runConnect] = useAction(true);
  const [cancelBusy, runCancel] = useAction();
  const cancelled = useRef(false);
  const [attempt, setAttempt] = useState(0);
  const [infoBusy, runInfo] = useAction(true);
  const [infoRefreshFailed, setInfoRefreshFailed] = useState(false);
  // The last failed action on this page, shown next to the control that ran it.
  const [failure, setFailure] = useState<{ at: string; message: string } | null>(null);
  const [forgetting, setForgetting] = useState(false);
  const [chosen, setTab] = useState<"settings" | "details">("settings");
  const d = entry.device;
  const adapter = state.adapters.find((a) => a.id === entry.adapterId);
  const connected = d.state === "connected";
  const settings = hasSettings(entry);
  const settingsWaited = useWaited(connected && !entry.settings?.current, `${attempt}/${d.settings_state === "discovering"}`);
  const infoWaited = useWaited(!entry.infoCurrent, 0);
  useEffect(() => {
    setFailure(null);
    setInfoRefreshFailed(false);
  }, [d.state]);
  const perform = async (at: string, runner: typeof run, action: Parameters<typeof run>[0]) => {
    setFailure(null);
    const result = await runner(action);
    if (!result.ok) setFailure({ at, message: result.message });
    return result;
  };
  const failed = (at: string) => (failure?.at === at ? failure.message : null);
  const failedText = (at: string) => (failure?.at === at ? <span className="error-text">{failure.message}</span> : undefined);
  const connecting = connectBusy || entry.pending.some((p) => p.command === "device.connect");
  const connect = async () => {
    cancelled.current = false;
    setFailure(null);
    const result = await runConnect({ type: "device.connect", key: entry.key });
    // Cancelling is the user's choice, not a failure.
    if (!result.ok && !cancelled.current && result.message !== codeText("cancelled")) setFailure({ at: "connection", message: result.message });
  };
  const cancel = () => {
    cancelled.current = true;
    void runCancel({ type: "device.connect.cancel", key: entry.key });
  };
  const tab = settings ? chosen : "details";
  const low = isLow(entry.battery, state.preferences.lowBatteryPercent);
  const battery = batteryText(entry.battery);
  const canConnect = d.effective_enabled && !d.blocked && d.pairing_state === "paired" && d.state === "disconnected";
  const info = (entry.info ?? []).filter((f) => f.available && f.key !== "name" && !f.key.startsWith("battery_"));
  const set = (type: "device.enabled" | "device.trusted" | "device.blocked" | "device.hidpp") => (value: boolean) =>
    void perform(type, run, { type, key: entry.key, value });
  const connectionProblem = failed("connection");

  return (
    <Page
      icon={<DeviceIcon kind={entry.kind} size={20} />}
      active={connected}
      title={entry.name}
      status={
        <>
          <Pill tone={connected ? "ok" : d.pairing_state === "needs_pairing" || d.blocked ? "warn" : "neutral"} dot>
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
      actions={
        <>
          {busy || connecting ? <Spinner /> : null}
          {connecting ? (
            <button disabled={cancelBusy} onClick={cancel}>
              Cancel
            </button>
          ) : connected || d.state === "connecting" ? (
            <button disabled={busy} onClick={() => void perform("connection", run, { type: "device.disconnect", key: entry.key })}>
              Disconnect
            </button>
          ) : (
            <button disabled={busy || !canConnect} onClick={() => void connect()}>
              Connect
            </button>
          )}
        </>
      }
    >
      {d.pairing_state === "needs_pairing" ? (
        <Banner kind="warning" action={<button onClick={onAdd}>Add Device…</button>}>
          Needs Pairing Again
        </Banner>
      ) : d.validation_error ? (
        <Banner kind="warning">{VALIDATION[d.validation_error]}</Banner>
      ) : null}
      {connectionProblem ? <Banner kind="error">{connectionProblem}</Banner> : null}
      {!d.effective_enabled && d.enabled_reason && d.enabled_reason !== "disabled" && d.enabled_reason !== "invalid" ? (
        <Banner>Inactive: {DISABLED[d.enabled_reason]}</Banner>
      ) : null}
      {(d.warnings ?? []).map((w) => (
        <Banner key={w}>{WARNINGS[w]}</Banner>
      ))}

      <Tabs
        label="Device"
        tabs={[
          ["settings", "Settings", !settings],
          ["details", "Details"],
        ]}
        value={tab}
        onChange={setTab}
      >
        {tab === "settings" ? (
          <Settings
            entry={entry}
            adapter={adapter}
            drafts={drafts}
            waited={settingsWaited}
            onRetry={() => {
              setAttempt((n) => n + 1);
              void act({ type: "settings.reload", key: entry.key });
            }}
          />
        ) : (
          <>
            <Card title="Connection">
              <SwitchRow
                title="Use This Device"
                subtitle={failedText("device.enabled")}
                checked={d.enabled}
                disabled={busy}
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
                subtitle={
                  failedText("device.hidpp") ??
                  (d.normalization_state === "unsupported" || d.normalization_state === "error" ? (
                    <>
                      {NORMALIZATION[d.normalization_state]}
                      {d.normalization_error ? ` — ${codeText(d.normalization_error)}` : ""}
                    </>
                  ) : undefined)
                }
                checked={d.hidpp_enabled}
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
                {info.map((f) => (
                  <Fact key={`${f.key}/${f.instance}`} label={infoLabel(f)} dim={!f.fresh || !entry.infoCurrent}>
                    {infoValue(f)}
                  </Fact>
                ))}
                {adapter ? <Fact label="Adapter">{adapter.name}</Fact> : null}
                <Fact label="Bluetooth">{TRANSPORTS[d.transport]}</Fact>
                {d.roles.length ? <Fact label="Input">{d.roles.map((r) => ROLES[r]).join(", ")}</Fact> : null}
                {connected && d.security
                  ? securityFacts(d.security).map(([label, value]) => (
                      <Fact key={label} label={label}>
                        {value}
                      </Fact>
                    ))
                  : null}
                <Fact label="Device ID">{d.device_id}</Fact>
              </Facts>
              {failed("info") ? (
                <Row title="Couldn't Read Information" subtitle={failedText("info")} />
              ) : infoWaited(GRACE_MS) && (entry.info === null || entry.infoError) ? (
                <Row title="Couldn't Read Information" />
              ) : entry.info === null ? (
                <Row title="Reading Information…">
                  <Spinner />
                </Row>
              ) : null}
              {connected ? (
                <div className="card-actions">
                  {infoBusy ? <Spinner /> : null}
                  <button disabled={infoBusy} onClick={async () => {
                    const result = await perform("info", runInfo, { type: "device.info.refresh", key: entry.key });
                    setInfoRefreshFailed(!result.ok);
                  }}>
                    <RefreshIcon /> {infoRefreshFailed || (infoWaited(GRACE_MS) && (entry.info === null || entry.infoError)) ? "Retry" : "Refresh"}
                  </button>
                </div>
              ) : null}
            </Card>

            <div className="actions">
              <button
                className="destructive"
                onClick={() => {
                  setFailure(null);
                  setForgetting(true);
                }}
              >
                Forget Device…
              </button>
            </div>
          </>
        )}
      </Tabs>

      <Dialog open={forgetting} title={`Forget “${entry.name}”?`} onClose={() => setForgetting(false)}>
        <p className="dialog-body">The adapter deletes its pairing and saved settings for this device.</p>
        {failed("unpair") ? <p className="dialog-body error-text">{failed("unpair")}</p> : null}
        <footer className="dialog-footer">
          <button onClick={() => setForgetting(false)}>Cancel</button>
          <button
            className="destructive"
            disabled={busy}
            onClick={async () => {
              const result = await perform("unpair", run, { type: "device.unpair", key: entry.key });
              if (result.ok) setForgetting(false);
            }}
          >
            Forget
          </button>
        </footer>
      </Dialog>
    </Page>
  );
}
