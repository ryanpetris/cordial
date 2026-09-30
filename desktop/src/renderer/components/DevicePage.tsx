import { useEffect, useRef, useState } from "react";
import type { Setting, SettingKey, SettingValue } from "../../protocol/types.ts";
import { isLow } from "../../shared/battery.ts";
import { hidppBusy, settingsBusy, settingsLive } from "../../shared/settings.ts";
import type { AppState, DeviceEntry } from "../../shared/state.ts";
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
  errorText,
  infoLabel,
  infoValue,
  securityFacts,
  settingOrder,
  settingsResultText,
  wheelInfo,
} from "../../shared/text.ts";
import { act, useAction } from "../api.ts";
import { Banner, Card, Dialog, Fact, Facts, Page, Pill, Row, Segmented, Spinner, SwitchRow, Tabs } from "./common.tsx";
import { BatteryGlyph, DeviceIcon, RefreshIcon, ResetIcon } from "./icons.tsx";

function valueText(s: Setting, v: SettingValue): string {
  if (v === null) return "Unknown";
  if (typeof v === "boolean") return v ? "On" : "Off";
  if (typeof v === "string") return choiceText(s.key, v);
  return String(v);
}

/** Whether a setting's choices fit side by side as joined buttons. */
const short = (s: Setting) =>
  s.choices.length <= 3 && s.choices.reduce<number>((n, c) => n + (typeof c === "string" ? choiceText(s.key, c) : String(c)).length, 0) <= 24;

/** An unsaved edit: the chosen value, or a free-form integer's text. */
export type Draft = boolean | number | string;

/** A device's unsaved setting edits. `set` with `expected` changes only a draft still equal to it. */
export interface Drafts {
  get(setting: SettingKey): Draft | undefined;
  set(setting: SettingKey, value: Draft | undefined, expected?: Draft): void;
}

const freeform = (s: Setting) => s.type === "integer" && !s.choices.length;

/** The value a draft saves, or null when it isn't a valid value. */
function draftValue(s: Setting, d: Draft): boolean | number | string | null {
  if (!freeform(s)) return d;
  const n = Number(d);
  if (String(d).trim() === "" || !Number.isSafeInteger(n)) return null;
  if ((s.min != null && n < s.min) || (s.max != null && n > s.max) || (n - (s.min ?? 0)) % (s.step ?? 1) !== 0) return null;
  return n;
}

/** One HID++ setting with a control that fits its metadata; edits stay a draft until Save. */
function SettingRow({ entry, s, drafts }: { entry: DeviceEntry; s: Setting; drafts: Drafts }) {
  const fresh = settingFresh(entry, s);
  const [saving, run] = useAction(true);
  const [feedback, setFeedback] = useState<{ ok: boolean; text: string } | null>(null);
  const info = SETTINGS[s.key];
  const draft = drafts.get(s.key);
  // A saved value is what the adapter keeps applying, so edits start from it.
  const base = s.managed ? s.desired : s.observed;
  const value = draft !== undefined && !freeform(s) ? draft : base;
  const next = draft === undefined ? null : draftValue(s, draft);
  const live = settingsLive(entry);
  const busy = settingsBusy(entry);
  const edit = (v: Draft) => {
    setFeedback(null);
    drafts.set(s.key, s.managed && !saving && !busy && draftValue(s, v) === base ? undefined : v);
  };
  const save = async () => {
    if (draft === undefined || next === null || !live || busy || saving) return;
    const result = await run({ type: "setting.set", key: entry.key, setting: s.key, value: next });
    if (result.ok) drafts.set(s.key, undefined, draft);
    setFeedback(result.ok ? { ok: true, text: "Saved" } : { ok: false, text: result.message });
  };
  const forget = async () => {
    const result = await run({ type: "setting.forget", key: entry.key, setting: s.key });
    if (result.ok && draft !== undefined) drafts.set(s.key, undefined, draft);
    setFeedback(result.ok ? null : { ok: false, text: result.message });
  };
  const locked = !live;
  let control: React.ReactNode;
  if (!s.writable) control = <span className="value">{valueText(s, s.observed)}</span>;
  else if (s.type === "bool")
    control = (
      <Segmented
        label={info.label}
        options={[
          [true, "On"],
          [false, "Off"],
        ]}
        value={typeof value === "boolean" ? value : null}
        selectCurrent={!s.managed || saving || busy}
        disabled={locked}
        onChange={edit}
      />
    );
  else if ((s.type === "enum" || s.choices.length) && value !== null && s.choices.includes(value) && short(s))
    control = (
      <Segmented
        label={info.label}
        options={s.choices.filter((c) => c !== null).map((c) => [c, typeof c === "string" ? choiceText(s.key, c) : String(c)])}
        value={value}
        selectCurrent={!s.managed || saving || busy}
        disabled={locked}
        onChange={edit}
      />
    );
  else if (s.type === "enum" || s.choices.length)
    control = (
      <select aria-label={info.label} value={value === null ? "" : String(value)} disabled={locked} onChange={(e) => {
        const choice = s.choices.find((c) => String(c) === e.target.value);
        if (choice !== undefined && choice !== null) edit(choice);
      }}>
        {value === null || !s.choices.includes(value) ? <option value={value === null ? "" : String(value)}>{value === null ? "Unknown" : valueText(s, value)}</option> : null}
        {s.choices.map((c) => (
          <option key={String(c)} value={String(c)}>
            {typeof c === "string" ? choiceText(s.key, c) : String(c)}
          </option>
        ))}
      </select>
    );
  else if (s.type === "integer")
    control = (
      <IntegerControl
        s={s}
        text={draft !== undefined ? String(draft) : typeof base === "number" ? String(base) : ""}
        invalid={draft !== undefined && next === null}
        disabled={locked}
        label={info.label}
        onEdit={edit}
        onSave={() => void save()}
        onRevert={() => drafts.set(s.key, undefined)}
      />
    );
  else control = <span className="value">{valueText(s, s.observed)}</span>;

  const notes: React.ReactNode[] = [];
  if (info.note) notes.push(info.note);
  if (s.writable && s.managed && s.state !== "applied") {
    notes.push(
      <span key="state" className={`chip ${s.state}`}>
        {SETTING_STATES[s.state]}
      </span>,
    );
    if (s.state === "changed_on_device") notes.push(` Device reports ${valueText(s, s.observed)}; saved ${valueText(s, s.desired)}.`);
  }
  if (s.error) notes.push(` ${codeText(s.error)}.`);
  if (feedback?.ok)
    notes.push(
      <span key="feedback" className="chip applied">
        {feedback.text}
      </span>,
    );
  else if (feedback)
    notes.push(
      <span key="feedback" className="error-text">
        {" "}
        {feedback.text}
      </span>,
    );
  return (
    <Row title={info.label} subtitle={notes.length ? <>{notes}</> : undefined} dim={!fresh}>
      {saving ? <Spinner /> : null}
      {control}
      {info.unit ? <span className="unit">{info.unit}</span> : null}
      {draft !== undefined ? (
        <button disabled={!live || busy || saving || next === null} onClick={() => void save()}>
          Save
        </button>
      ) : null}
      {s.writable && s.managed ? (
        <button
          className="icon-button"
          title="Forget Saved Value"
          aria-label={`Forget saved ${info.label}`}
          disabled={busy || saving}
          onClick={() => void forget()}
        >
          <ResetIcon />
        </button>
      ) : null}
    </Row>
  );
}

function IntegerControl({
  s,
  text,
  invalid,
  disabled,
  label,
  onEdit,
  onSave,
  onRevert,
}: {
  s: Setting;
  text: string;
  invalid: boolean;
  disabled: boolean;
  label: string;
  onEdit: (text: string) => void;
  onSave: () => void;
  onRevert: () => void;
}) {
  const step = s.step ?? 1;
  const min = s.min ?? undefined;
  const max = s.max ?? undefined;
  const slider = min !== undefined && max !== undefined && (max - min) / step <= 50;
  return slider ? (
    <span className="slider">
      <input
        type="range"
        aria-label={label}
        min={min}
        max={max}
        step={step}
        value={text || min}
        disabled={disabled}
        onChange={(e) => onEdit(e.target.value)}
      />
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
        if (e.key === "Enter") onSave();
        if (e.key === "Escape") onRevert();
      }}
    />
  );
}

/** Whether a row shows a current reading: a row can stay marked fresh after
 * its list was invalidated or the device stopped reporting. */
const settingFresh = (entry: DeviceEntry, s: Setting) =>
  s.fresh && !!entry.settings?.current && entry.device.state === "connected" && entry.device.normalization_state !== "resetting";

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

function Settings({ entry, drafts, waited, onRetry, refreshFailed, onRefreshed }: {
  entry: DeviceEntry;
  drafts: Drafts;
  waited: (ms: number) => boolean;
  onRetry: () => void;
  refreshFailed: boolean | null;
  onRefreshed: (ok: boolean) => void;
}) {
  const [busy, run] = useAction();
  const d = entry.device;
  const view = entry.settings;
  const settings = [...(view?.settings ?? [])].sort((a, b) => settingOrder(a.key) - settingOrder(b.key));
  const categories = [...new Set(settings.map((s) => SETTINGS[s.key].category))];
  const managed = settings.some((s) => s.managed);
  const live = settingsLive(entry);
  const blocked = busy || settingsBusy(entry) || !live;
  // The adapter discovering a device's settings is progress, so it gets longer.
  const failing = waited(d.settings_state === "discovering" ? DISCOVERY_MS : GRACE_MS);
  const problem = (
    <>
      <span className="muted">Couldn't read the device's settings.</span>
      <button onClick={onRetry}>Retry</button>
    </>
  );

  if (!settings.length)
    return d.settings_state === "unsupported" || d.settings_state === "error" ? (
      <p className="muted">Settings unavailable{d.settings_error ? `: ${codeText(d.settings_error)}.` : "."}</p>
    ) : failing ? (
      <div className="panel-state">{problem}</div>
    ) : (
      <div className="panel-state">
        <Spinner />
        <span className="muted">Reading the device's settings…</span>
      </div>
    );

  const loadFailed = failing && view?.loadError;
  const note = view?.result ? <span className={view.result.error ? "error-text" : "muted"}>{settingsResultText(view.result)}</span>
    : d.settings_error ? <span className="error-text">{codeText(d.settings_error)}</span>
    : null;
  return (
    <>
      {categories.map((category) => {
        const rows = settings.filter((s) => SETTINGS[s.key].category === category);
        const wheel = rows.find((s) => s.key === "wheel.info");
        return (
          <Card key={category} title={category}>
            {rows
              .filter((s) => s !== wheel)
              .map((s) => (
                <SettingRow key={s.key} entry={entry} s={s} drafts={drafts} />
              ))}
            {wheel ? <WheelInfo entry={entry} s={wheel} /> : null}
          </Card>
        );
      })}
      <div className="actions">
        {note || loadFailed ? <span className="actions-note">{note}{loadFailed ? problem : null}</span> : null}
        {busy || d.settings_state === "applying" || d.settings_state === "discovering" ? <Spinner /> : null}
        {!loadFailed ? (
          <button disabled={blocked} onClick={async () => {
            const result = await run({ type: "settings.refresh", key: entry.key });
            onRefreshed(result.ok);
          }}>
            <RefreshIcon /> {(refreshFailed ?? (view?.result?.kind === "refresh" && !!view.result.error)) ? "Retry" : "Refresh"}
          </button>
        ) : null}
        {managed && d.hidpp_enabled ? (
          <button disabled={blocked} onClick={() => void run({ type: "settings.apply", key: entry.key })}>
            Apply Saved Settings
          </button>
        ) : null}
      </div>
    </>
  );
}

export function DevicePage({ state, entry, drafts, onAdd }: { state: AppState; entry: DeviceEntry; drafts: Drafts; onAdd: () => void }) {
  const [busy, run] = useAction(true);
  const [connectBusy, runConnect] = useAction(true);
  const [cancelBusy, runCancel] = useAction();
  const cancelled = useRef(false);
  const [attempt, setAttempt] = useState(0);
  const [infoBusy, runInfo] = useAction(true);
  const [refreshFailed, setRefreshFailed] = useState<boolean | null>(null);
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
    setRefreshFailed(null);
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
  const lastError =
    d.last_error && !connected && d.state !== "connecting" && !connecting && d.last_error.code !== "cancelled"
      ? `Last connection failed: ${errorText(d.last_error)}`
      : null;
  const connectionProblem = failed("connection") ?? lastError;

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
          Needs pairing again.
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
            drafts={drafts}
            waited={settingsWaited}
            refreshFailed={refreshFailed}
            onRefreshed={(ok) => setRefreshFailed(!ok)}
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
                title="Allow Automatic Connections"
                subtitle={failedText("device.trusted")}
                checked={d.trusted}
                disabled={busy}
                onChange={set("device.trusted")}
              />
              <SwitchRow
                title="Logitech Enhancements"
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
                disabled={busy || hidppBusy(entry)}
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
                <Row title="Couldn't read information" subtitle={failedText("info")} />
              ) : infoWaited(GRACE_MS) && (entry.info === null || entry.infoError) ? (
                <Row title="Couldn't read information" />
              ) : entry.info === null ? (
                <Row title="Reading information…">
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
