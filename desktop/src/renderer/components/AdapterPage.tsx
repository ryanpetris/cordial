import { useEffect, useId, useRef, useState } from "react";
import { isLow } from "../../shared/battery.ts";
import { adapterName } from "../../shared/adapter-name.ts";
import { resource } from "../../shared/capacity.ts";
import { stagedInterfaces } from "../../shared/profiles.ts";
import type { AdapterChanges, AdapterEntry, AdapterStatus, AppState, HostPlatform, InterfaceChange, TransportName } from "../../shared/state.ts";
import {
  PLATFORMS,
  READ_FAILED,
  STORAGE_FULL,
  TRANSPORTS,
  USB_CAPACITY,
  adapterStatus,
  batteryStale,
  capacityText,
  connected as isConnected,
  deviceStatus,
  errorText,
  hasError,
  reasonText,
  storageFull,
} from "../../shared/text.ts";
import { useAction } from "../api.ts";
import type { Selection } from "../App.tsx";
import { Banner, Card, Dialog, Fact, Facts, Meter, Page, Pill, Row, Segmented, Spinner, Staged, SwitchRow, TabBar, TabPanel } from "./common.tsx";
import { AdapterProfiles, ConfigurationInterfaces, ProfileMemory } from "./Profiles.tsx";
import { AdapterIcon, CheckIcon, ChevronIcon, CloseIcon, DeviceIcon, PencilIcon, PlugIcon, RefreshIcon, SwapIcon, UndoIcon, UnplugIcon } from "./icons.tsx";

/** The rename dialog's contents; mounted each time it opens, so it starts from the current name. */
function Rename({ adapter, busy, error, onRename, onDone }: {
  adapter: AdapterEntry;
  busy: boolean;
  error: string | null;
  onRename: (name: string | null) => Promise<void>;
  onDone: () => void;
}) {
  const [name, setName] = useState(adapter.name);
  const available = adapter.connection === "connected" && !!adapter.status?.ready;
  const valid = adapterName(name) !== null;
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.select(), []);
  return (
    <form
      onSubmit={async (e) => {
        e.preventDefault();
        if (!available || !valid || busy) return;
        await onRename(name);
      }}
    >
      <input ref={input} className="name-input" aria-label="Adapter name" value={name} disabled={busy} maxLength={64} onChange={(e) => setName(e.target.value)} />
      {error ? <p className="dialog-body error-text">{error}</p> : null}
      <footer className="dialog-footer">
        <button type="button" disabled={busy || !available} onClick={() => void onRename(null)}>
          <UndoIcon /> Reset to Default
        </button>
        <button type="button" disabled={busy} onClick={onDone}>
          <CloseIcon /> Cancel
        </button>
        <button type="submit" className="suggested" disabled={busy || !available || !valid}>
          <PencilIcon /> Rename
        </button>
      </footer>
    </form>
  );
}

type AdapterTab = "details" | "settings" | "profiles" | "diagnostics";

const ADAPTER_TABS: [AdapterTab, string][] = [
  ["details", "Details"],
  ["settings", "Settings"],
  ["profiles", "Profiles"],
  ["diagnostics", "Diagnostics"],
];

/** The staged values in `draft` that differ from what `status` reports. */
function pending(draft: AdapterChanges, status: AdapterStatus): AdapterChanges {
  const changes: AdapterChanges = {};
  if (draft.platform !== undefined && draft.platform !== status.platform) changes.platform = draft.platform;
  const transports = Object.entries(draft.transports ?? {}).filter(([t, on]) =>
    status.transports.some((x) => x.transport === t && x.enabled !== on));
  if (transports.length) changes.transports = Object.fromEntries(transports);
  const interfaces: Record<string, InterfaceChange> = {};
  for (const i of status.interfaces) {
    const staged = draft.interfaces?.[i.interface];
    const change: InterfaceChange = {};
    if (staged?.enabled !== undefined && staged.enabled !== i.enabled) change.enabled = staged.enabled;
    if (staged?.profile !== undefined && staged.profile !== i.profile) change.profile = staged.profile;
    if (Object.keys(change).length) interfaces[i.interface] = change;
  }
  if (Object.keys(interfaces).length) changes.interfaces = interfaces;
  return changes;
}

/** `draft` without the values `sent` saved; values staged again since stay. */
function settle(draft: AdapterChanges, sent: AdapterChanges): AdapterChanges {
  const next = { ...draft };
  if (sent.platform !== undefined && next.platform === sent.platform) delete next.platform;
  const transports = Object.entries(next.transports ?? {}).filter(([t, on]) => sent.transports?.[t as TransportName] !== on);
  if (transports.length) next.transports = Object.fromEntries(transports);
  else delete next.transports;
  const interfaces = Object.entries(next.interfaces ?? {}).flatMap(([i, change]) => {
    const rest = Object.fromEntries(Object.entries(change).filter(([k, v]) => sent.interfaces?.[i]?.[k as keyof InterfaceChange] !== v));
    return Object.keys(rest).length ? [[i, rest]] : [];
  });
  if (interfaces.length) next.interfaces = Object.fromEntries(interfaces);
  else delete next.interfaces;
  return next;
}

export function AdapterPage({
  state,
  adapter,
  renaming,
  draft,
  onDraft,
  onRenamed,
  onSelect,
}: {
  state: AppState;
  adapter: AdapterEntry;
  renaming: boolean;
  /** The adapter's staged settings, kept by the window across tabs and pages. */
  draft: AdapterChanges;
  onDraft: (update: (draft: AdapterChanges) => AdapterChanges) => void;
  onRenamed: () => void;
  onSelect: (s: Selection) => void;
}) {
  const [busy, run] = useAction();
  const [quietBusy, runQuiet] = useAction(true);
  const [saving, runSave] = useAction(true);
  const [renameError, setRenameError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [connectError, setConnectError] = useState<string | null>(null);
  const [editing, setEditing] = useState(renaming);
  useEffect(() => setEditing(renaming), [renaming]);
  const [chosen, setTab] = useState<AdapterTab>("details");
  const tabs = useId();
  const s = adapter.status;
  const connected = adapter.connection === "connected";
  const ready = connected && !!s?.ready;
  // The status the tabs show: the adapter's own once it is ready, and the last one it reported
  // while it reconnects, shown with its controls disabled. Null while a new session starts.
  const view = s?.ready ? s : null;
  // Whether the adapter has profiles, kept while its status is unknown so a reconnect leaves the
  // Profiles tab in place.
  const known = view ? !!view.profileSupport : null;
  const [profiles, setProfiles] = useState(known ?? false);
  if (known !== null && known !== profiles) setProfiles(known);
  const hasTab = (t: AdapterTab) => t !== "profiles" || profiles;
  const tab = hasTab(chosen) ? chosen : "details";
  const closeRename = () => {
    setEditing(false);
    setRenameError(null);
    onRenamed();
  };
  const done = () => { if (!quietBusy) closeRename(); };
  const status = adapterStatus(adapter);
  // Settings and Profiles edits stage in `draft` and go to the adapter together with Save.
  const changes = view ? pending(draft, view) : {};
  const dirty = Object.keys(changes).length > 0;
  const locked = !ready || saving;
  const canSave = !locked && dirty;
  const stage = (update: AdapterChanges) => {
    if (!view) return;
    setSaveError(null);
    onDraft((d) => {
      const interfaces = { ...d.interfaces };
      for (const [i, change] of Object.entries(update.interfaces ?? {})) interfaces[i] = { ...interfaces[i], ...change };
      return pending({ ...d, ...update, transports: { ...d.transports, ...update.transports }, interfaces }, view);
    });
  };
  const platform = draft.platform ?? view?.platform;
  const save = async () => {
    if (!canSave) return;
    setSaveError(null);
    const sent = changes;
    const result = await runSave({ type: "adapter.settings", adapterId: adapter.id, ...sent });
    if (result.ok) onDraft((d) => settle(d, sent));
    else setSaveError(result.message);
  };
  const requestSave = () => { void save(); };
  // A save whose outcome was unknown no longer is once the adapter reports the staged values.
  useEffect(() => {
    if (view && !dirty) setSaveError(null);
  }, [view, dirty]);
  const discard = () => {
    setSaveError(null);
    onDraft(() => ({}));
  };
  const staging = tab === "settings" || tab === "profiles";

  // Ctrl+S or Cmd+S saves while Settings or Profiles is shown and no dialog is open.
  const saveRef = useRef<(() => void) | null>(null);
  saveRef.current = staging ? requestSave : null;
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
  const connect = async () => {
    setConnectError(null);
    const result = await runQuiet({ type: "adapter.connect", adapterId: adapter.id });
    if (!result.ok) setConnectError(result.message);
  };
  const connectFailure = connected ? null : (adapter.connectError ?? connectError);
  const devices = state.devices.filter((d) => d.adapterId === adapter.id);
  const threshold = state.preferences.lowBatteryPercent;
  const transports = view?.transports ?? [];
  // Details reports the adapter's live state, so it shows only while connected.
  const live = connected ? view : null;
  const usbInterfaces = resource(live?.resources, "interfaces");
  const usbErrors = USB_CAPACITY.filter(([code]) => hasError(live?.errors, code));
  const enabledDevices = live?.transports.flatMap((t) => {
    const r = resource(t.resources, "devices");
    return t.enabled && r && (r.count !== null || r.limit !== null) ? [{ transport: t.transport, r }] : [];
  }) ?? [];
  const empty = (
    <div className="page-empty">
      <AdapterIcon size={64} />
      <h2>{status.text}</h2>
    </div>
  );

  return (
    <Page
      icon={<AdapterIcon size={20} />}
      active={connected}
      title={adapter.name}
      status={
        <Pill tone={status.warn ? "warn" : connected && adapter.readiness === "ready" ? "ok" : "neutral"} dot>
          {status.text}
        </Pill>
      }
      nav={
        <TabBar
          id={tabs}
          label="Adapter"
          tabs={ADAPTER_TABS.filter(([t]) => hasTab(t))}
          value={tab}
          onChange={setTab}
        />
      }
      bar={
        staging ? (
          <>
            <span className="bar-start">
              {saving ? <Spinner /> : null}
              {saveError ? <span className="error-text">{saveError}</span> : null}
            </span>
            <button disabled={saving || !dirty} onClick={discard}>
              <UndoIcon /> Discard
            </button>
            <button className="suggested" disabled={!canSave} onClick={requestSave}>
              <CheckIcon /> Save
            </button>
          </>
        ) : (
          <>
            {connectFailure ? <span className="bar-start error-text">{connectFailure}</span> : null}
            {busy || quietBusy || adapter.connection === "connecting" ? <Spinner /> : null}
            <button disabled={!ready} onClick={() => setEditing(true)}>
              <PencilIcon /> Rename
            </button>
            {connected ? (
              <button disabled={busy} onClick={() => void run({ type: "adapter.disconnect", adapterId: adapter.id })}>
                <UnplugIcon /> Disconnect
              </button>
            ) : adapter.connection === "disconnected" ? (
              <button className="suggested" disabled={quietBusy} onClick={() => void connect()}>
                <PlugIcon /> Connect
              </button>
            ) : null}
          </>
        )
      }
    >
      <Dialog open={editing} title="Rename Adapter" onClose={done}>
        <Rename adapter={adapter} busy={quietBusy} error={renameError} onDone={done} onRename={async (name) => {
          setRenameError(null);
          const result = await runQuiet({ type: "adapter.name", adapterId: adapter.id, name });
          if (result.ok) closeRename();
          else setRenameError(result.message);
        }} />
      </Dialog>
      {adapter.attention.map((a) => (
        <Banner key={a} kind="warning">
          {a}
        </Banner>
      ))}

      <TabPanel id={tabs} value={tab}>
        {tab === "settings" ? (
          view ? (
            <>
              <Card>
                <div className="row">
                  <div className="row-text">
                    <div className="row-title">Platform</div>
                  </div>
                  <div className="row-end">
                    {changes.platform !== undefined ? <Staged label="Platform" /> : null}
                    <Segmented
                      label="Platform"
                      options={Object.entries(PLATFORMS) as [HostPlatform, string][]}
                      value={platform ?? null}
                      disabled={locked}
                      onChange={(p) => stage({ platform: p })}
                    />
                  </div>
                </div>
                {platform !== state.hostPlatform ? (
                  <Banner action={<button disabled={locked} onClick={() => stage({ platform: state.hostPlatform })}><SwapIcon /> Switch to {PLATFORMS[state.hostPlatform]}</button>}>
                    This computer runs {state.hostPlatform === "mac" ? "macOS" : PLATFORMS[state.hostPlatform]}.
                  </Banner>
                ) : null}
              </Card>
              {transports.length ? (
                <Card title="Bluetooth">
                  {transports.map(({ transport, enabled }) => (
                    <SwitchRow
                      key={transport}
                      title={TRANSPORTS[transport]}
                      checked={draft.transports?.[transport] ?? enabled}
                      disabled={locked}
                      end={changes.transports?.[transport] !== undefined ? <Staged label={TRANSPORTS[transport]} /> : null}
                      onChange={(value) => stage({ transports: { [transport]: value } })}
                    />
                  ))}
                </Card>
              ) : null}
            </>
          ) : (
            empty
          )
        ) : tab === "profiles" ? (
          view ? (
            <>
              <ProfileMemory status={view} />
              <ConfigurationInterfaces
                adapter={adapter}
                interfaces={stagedInterfaces(view, draft)}
                changed={changes.interfaces ?? {}}
                locked={locked}
                onChange={(i, change) => stage({ interfaces: { [i]: change } })}
              />
              <AdapterProfiles adapter={adapter} locked={locked} />
            </>
          ) : (
            empty
          )
        ) : tab === "diagnostics" ? (
          <>
            {connected && s?.errors.length ? (
              <Card title="Errors">
                {s.errors.map((e) => (
                  <Row key={JSON.stringify(e)} title={errorText(e)} />
                ))}
              </Card>
            ) : null}
            {adapter.unreadableDevices.length ? (
              <Card
                title="Saved Devices"
                footer={
                  <button disabled={busy || !connected} onClick={() => void run({ type: "devices.reload", adapterId: adapter.id })}>
                    <RefreshIcon /> Retry
                  </button>
                }
              >
                {adapter.unreadableDevices.map(({ id, error }) => {
                  const detail = errorText(error, "read");
                  return <Row key={id} title={`Device ${id}`} subtitle={detail === READ_FAILED ? "Couldn't Read" : `Couldn't Read: ${reasonText(detail)}`} dim />;
                })}
              </Card>
            ) : null}
            <Card title="Identifiers">
              <Facts>
                <Fact label="Adapter ID">{adapter.id}</Fact>
              </Facts>
            </Card>
          </>
        ) : (
          <>
            {live ? null : empty}
            {devices.length ? (
              <Card title="Devices">
                {devices.map((d) => {
                  const low = isLow(d.battery, threshold);
                  return (
                    <button key={d.key} className="row clickable" onClick={() => onSelect({ page: "device", key: d.key })}>
                      <span className={isConnected(d) ? "row-icon on" : "row-icon"}>
                        <DeviceIcon kind={d.kind} />
                      </span>
                      <span className="row-text">
                        <span className="row-title strong">{d.name}</span>
                        <span className="row-subtitle">{deviceStatus(d)}</span>
                      </span>
                      {d.battery?.percent != null ? <span className={`value${low ? " low" : ""}${batteryStale(d.battery) ? " dim" : ""}`}>{d.battery.percent}%</span> : null}
                      <ChevronIcon />
                    </button>
                  );
                })}
              </Card>
            ) : null}

            {enabledDevices.length ? (
              <Card title="Active Devices">
                {enabledDevices.map(({ transport, r }) => (
                  <div key={transport} className="row meter-row">
                    <div className="meter-line">
                      <span>{TRANSPORTS[transport]}</span>
                      <span className="value">{capacityText(r)}</span>
                    </div>
                    {r.count !== null && r.limit !== null ? <Meter fraction={r.limit ? Math.min(1, r.count / r.limit) : 0} /> : null}
                  </div>
                ))}
              </Card>
            ) : null}

            {usbInterfaces || usbErrors.length ? (
              <Card title="USB">
                <Facts>
                  <Fact label="Interfaces">
                    {usbInterfaces ? capacityText(usbInterfaces) : "Unknown"}
                    {usbErrors.map(([code, label]) => (
                      <span key={code} className="fact-detail">
                        {label}
                      </span>
                    ))}
                  </Fact>
                </Facts>
              </Card>
            ) : null}

            {live && live.transports.some((t) => t.enabled) ? (
              <Card title="New Pairings">
                <Facts>
                  {live.transports.filter((t) => t.enabled).map(({ transport }) => (
                    <Fact key={transport} label={TRANSPORTS[transport]}>
                      {storageFull(live) ? STORAGE_FULL : "Available"}
                    </Fact>
                  ))}
                </Facts>
              </Card>
            ) : null}

            {live ? (
              <Card title="Information">
                <Facts>
                  <Fact label="Firmware">{live.information.firmwareVersion ?? "Unknown"}</Fact>
                  {live.information.boardName !== null ? <Fact label="Board">{live.information.boardName}</Fact> : null}
                </Facts>
              </Card>
            ) : null}
          </>
        )}
      </TabPanel>
    </Page>
  );
}
