import { useState } from "react";
import type { Setting, SettingValue } from "../../protocol/types.ts";
import { isLow } from "../../shared/battery.ts";
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
  batteryText,
  choiceText,
  codeText,
  deviceStatus,
  errorText,
  infoLabel,
  infoValue,
  securityFacts,
  settingOrder,
  wheelInfo,
} from "../../shared/text.ts";
import { useAction } from "../api.ts";
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

/** One HID++ setting with a control that fits its metadata. */
function SettingRow({ entry, s }: { entry: DeviceEntry; s: Setting }) {
  const [busy, run] = useAction();
  const info = SETTINGS[s.key];
  const set = (value: boolean | number | string) => void run({ type: "setting.set", key: entry.key, setting: s.key, value });
  const current = s.managed && s.state !== "changed_on_device" ? s.desired : s.observed;
  const disabled = busy || s.state === "applying";
  let control: React.ReactNode;
  if (!s.writable) control = <span className="value">{valueText(s, s.observed)}</span>;
  else if (s.type === "bool")
    control = (
      <input
        type="checkbox"
        role="switch"
        className="switch"
        aria-label={info.label}
        checked={current === true}
        disabled={disabled}
        onChange={(e) => set(e.target.checked)}
      />
    );
  else if ((s.type === "enum" || s.choices.length) && current !== null && s.choices.includes(current) && short(s))
    control = (
      <Segmented
        label={info.label}
        options={s.choices.filter((c) => c !== null).map((c) => [c, typeof c === "string" ? choiceText(s.key, c) : String(c)])}
        value={current}
        disabled={disabled}
        onChange={set}
      />
    );
  else if (s.type === "enum" || s.choices.length)
    control = (
      <select aria-label={info.label} value={current === null ? "" : String(current)} disabled={disabled} onChange={(e) => {
        const choice = s.choices.find((c) => String(c) === e.target.value);
        if (choice !== undefined && choice !== null) set(choice);
      }}>
        {current === null || !s.choices.includes(current) ? <option value={current === null ? "" : String(current)}>{current === null ? "Unknown" : valueText(s, current)}</option> : null}
        {s.choices.map((c) => (
          <option key={String(c)} value={String(c)}>
            {typeof c === "string" ? choiceText(s.key, c) : String(c)}
          </option>
        ))}
      </select>
    );
  else if (s.type === "integer") control = <IntegerControl s={s} value={current} disabled={disabled} label={info.label} onSet={set} />;
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
  return (
    <Row title={info.label} subtitle={notes.length ? <>{notes}</> : undefined} dim={!s.fresh}>
      {busy ? <Spinner /> : null}
      {control}
      {info.unit ? <span className="unit">{info.unit}</span> : null}
      {s.writable && s.managed ? (
        <button
          className="icon-button"
          title="Forget Saved Value"
          aria-label={`Forget saved ${info.label}`}
          disabled={busy}
          onClick={() => void run({ type: "setting.forget", key: entry.key, setting: s.key })}
        >
          <ResetIcon />
        </button>
      ) : null}
    </Row>
  );
}

function IntegerControl({
  s,
  value,
  disabled,
  label,
  onSet,
}: {
  s: Setting;
  value: SettingValue;
  disabled: boolean;
  label: string;
  onSet: (v: number) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const step = s.step ?? 1;
  const min = s.min ?? undefined;
  const max = s.max ?? undefined;
  const shown = draft ?? (typeof value === "number" ? String(value) : "");
  const commit = (text: string) => {
    setDraft(null);
    const n = Number(text);
    if (text === "" || !Number.isInteger(n) || n === value) return;
    if ((min !== undefined && n < min) || (max !== undefined && n > max) || (n - (min ?? 0)) % step !== 0) return;
    onSet(n);
  };
  const slider = min !== undefined && max !== undefined && (max - min) / step <= 50;
  return slider ? (
    <span className="slider">
      <input
        type="range"
        aria-label={label}
        min={min}
        max={max}
        step={step}
        value={shown || min}
        disabled={disabled}
        onChange={(e) => setDraft(e.target.value)}
        onPointerUp={(e) => commit((e.target as HTMLInputElement).value)}
        onKeyUp={(e) => commit((e.target as HTMLInputElement).value)}
      />
      <output>{shown}</output>
    </span>
  ) : (
    <input
      className="number"
      type="number"
      aria-label={label}
      min={min}
      max={max}
      step={step}
      value={shown}
      disabled={disabled}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={(e) => commit(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit((e.target as HTMLInputElement).value);
        if (e.key === "Escape") setDraft(null);
      }}
    />
  );
}

/** The read-only wheel capability readout as a strip of figures. */
function WheelInfo({ s }: { s: Setting }) {
  const facts = wheelInfo(s.observed, s.feature_version);
  if (!facts)
    return (
      <Row title={SETTINGS[s.key].label} dim={!s.fresh}>
        <span className="value">{s.observed === null ? "Unknown" : `Unrecognized (${String(s.observed)})`}</span>
      </Row>
    );
  return (
    <div className={s.fresh ? "figures" : "figures dim"}>
      {facts.map(([label, value]) => (
        <span key={label} className="figure">
          <span className="figure-value">{value}</span>
          <span className="figure-label">{label}</span>
        </span>
      ))}
    </div>
  );
}

/** Whether the settings tab has anything to show. */
function hasSettings(entry: DeviceEntry) {
  const d = entry.device;
  const view = entry.settings;
  if (d.state !== "connected" || !view) return false;
  return view.settings.length > 0 || view.loadError !== null || d.settings_error !== null || reading(entry);
}

const reading = (entry: DeviceEntry) =>
  entry.device.settings_state === "discovering" || entry.device.settings_state === "pending" || !entry.settings?.current;

function Settings({ entry }: { entry: DeviceEntry }) {
  const [busy, run] = useAction();
  const d = entry.device;
  const view = entry.settings!;
  const settings = [...view.settings].sort((a, b) => settingOrder(a.key) - settingOrder(b.key));
  const categories = [...new Set(settings.map((s) => SETTINGS[s.key].category))];
  const managed = settings.some((s) => s.managed);
  return (
    <>
      {view.loadError ? <Banner kind="error">Couldn't read the device's settings: {view.loadError}</Banner> : null}
      {categories.map((category) => {
        const rows = settings.filter((s) => SETTINGS[s.key].category === category);
        const wheel = rows.find((s) => s.key === "wheel.info");
        return (
          <Card key={category} title={category}>
            {rows
              .filter((s) => s !== wheel)
              .map((s) => (
                <SettingRow key={s.key} entry={entry} s={s} />
              ))}
            {wheel ? <WheelInfo s={wheel} /> : null}
          </Card>
        );
      })}
      {settings.length === 0 && !view.loadError ? (
        reading(entry) ? (
          <p className="muted">Reading the device's settings…</p>
        ) : d.settings_error ? (
          <p className="muted">Settings unavailable: {codeText(d.settings_error)}.</p>
        ) : null
      ) : null}
      {settings.length ? (
        <div className="actions">
          {busy ? <Spinner /> : null}
          <button disabled={busy} onClick={() => void run({ type: "settings.refresh", key: entry.key })}>
            <RefreshIcon /> Read Again
          </button>
          {managed && d.hidpp_enabled ? (
            <button disabled={busy} onClick={() => void run({ type: "settings.apply", key: entry.key })}>
              Apply Saved Settings
            </button>
          ) : null}
        </div>
      ) : null}
    </>
  );
}

export function DevicePage({ state, entry, onAdd }: { state: AppState; entry: DeviceEntry; onAdd: () => void }) {
  const [busy, run] = useAction();
  const [infoBusy, runInfo] = useAction();
  const [forgetting, setForgetting] = useState(false);
  const [chosen, setTab] = useState<"settings" | "details">("settings");
  const d = entry.device;
  const adapter = state.adapters.find((a) => a.id === entry.adapterId);
  const connected = d.state === "connected";
  const settings = hasSettings(entry);
  const tab = settings ? chosen : "details";
  const low = isLow(entry.battery, state.preferences.lowBatteryPercent);
  const battery = batteryText(entry.battery);
  const canConnect = d.effective_enabled && !d.blocked && d.pairing_state === "paired" && d.state === "disconnected";
  const info = (entry.info ?? []).filter((f) => f.available && f.key !== "name" && !f.key.startsWith("battery_"));
  const set = (type: "device.enabled" | "device.trusted" | "device.blocked" | "device.hidpp") => (value: boolean) =>
    void run({ type, key: entry.key, value });

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
            <Pill tone={low ? "low" : "neutral"}>
              <BatteryGlyph percent={entry.battery.percent} charging={entry.battery.charging} low={low} />
              {battery}
            </Pill>
          ) : null}
        </>
      }
      actions={
        <>
          {busy ? <Spinner /> : null}
          {connected || d.state === "connecting" ? (
            <button disabled={busy} onClick={() => void run({ type: "device.disconnect", key: entry.key })}>
              Disconnect
            </button>
          ) : (
            <button disabled={busy || !canConnect} onClick={() => void run({ type: "device.connect", key: entry.key })}>
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
      {d.last_error && !connected ? <Banner kind="error">Last connection failed: {errorText(d.last_error)}</Banner> : null}
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
          <Settings entry={entry} />
        ) : (
          <>
            <Card title="Connection">
              <SwitchRow title="Use This Device" checked={d.enabled} disabled={busy} onChange={set("device.enabled")} />
              <SwitchRow title="Allow Automatic Connections" checked={d.trusted} disabled={busy} onChange={set("device.trusted")} />
              <SwitchRow
                title="Logitech Enhancements"
                subtitle={
                  d.normalization_state === "unsupported" || d.normalization_state === "error" ? (
                    <>
                      {NORMALIZATION[d.normalization_state]}
                      {d.normalization_error ? ` — ${codeText(d.normalization_error)}` : ""}
                    </>
                  ) : undefined
                }
                checked={d.hidpp_enabled}
                disabled={busy}
                onChange={set("device.hidpp")}
              />
              <SwitchRow title="Block Connections" checked={d.blocked} disabled={busy} onChange={set("device.blocked")} />
            </Card>

            <Card title="Information">
              <Facts>
                {info.map((f) => (
                  <Fact key={`${f.key}/${f.instance}`} label={infoLabel(f)} dim={!f.fresh}>
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
              {entry.infoError ? (
                <Row title="Couldn't read information" subtitle={entry.infoError} />
              ) : entry.info === null ? (
                <Row title="Reading information…" />
              ) : null}
              {connected ? (
                <div className="card-actions">
                  {infoBusy ? <Spinner /> : null}
                  <button disabled={infoBusy} onClick={() => void runInfo({ type: "device.info.refresh", key: entry.key })}>
                    <RefreshIcon /> Update
                  </button>
                </div>
              ) : null}
            </Card>

            <div className="actions">
              <button className="destructive" onClick={() => setForgetting(true)}>
                Forget Device…
              </button>
            </div>
          </>
        )}
      </Tabs>

      <Dialog open={forgetting} title={`Forget “${entry.name}”?`} onClose={() => setForgetting(false)}>
        <p className="dialog-body">The adapter deletes its pairing and saved settings for this device.</p>
        <footer className="dialog-footer">
          <button onClick={() => setForgetting(false)}>Cancel</button>
          <button
            className="destructive"
            disabled={busy}
            onClick={async () => {
              const result = await run({ type: "device.unpair", key: entry.key });
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
