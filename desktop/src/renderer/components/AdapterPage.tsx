import { useEffect, useRef, useState } from "react";
import { isLow } from "../../shared/battery.ts";
import { adapterName } from "../../shared/adapter-name.ts";
import type { AdapterEntry, AppState, HostPlatform } from "../../shared/state.ts";
import { PLATFORMS, STORAGE_FULL, TRANSPORTS, adapterStatus, batteryStale, deviceStatus, infoOf, storageFull } from "../../shared/text.ts";
import { useAction } from "../api.ts";
import type { Selection } from "../App.tsx";
import { Banner, Card, Dialog, Fact, Facts, Meter, Page, Pill, Segmented, Spinner } from "./common.tsx";
import { AdapterIcon, ChevronIcon, CloseIcon, DeviceIcon, PencilIcon, PlugIcon, SwapIcon, UndoIcon, UnplugIcon } from "./icons.tsx";

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

export function AdapterPage({
  state,
  adapter,
  renaming,
  onRenamed,
  onSelect,
}: {
  state: AppState;
  adapter: AdapterEntry;
  renaming: boolean;
  onRenamed: () => void;
  onSelect: (s: Selection) => void;
}) {
  const [busy, run] = useAction();
  const [quietBusy, runQuiet] = useAction(true);
  const [renameError, setRenameError] = useState<string | null>(null);
  const [platformError, setPlatformError] = useState<string | null>(null);
  const [connectError, setConnectError] = useState<string | null>(null);
  const [editing, setEditing] = useState(renaming);
  useEffect(() => setEditing(renaming), [renaming]);
  const s = adapter.status;
  const connected = adapter.connection === "connected";
  const closeRename = () => {
    setEditing(false);
    setRenameError(null);
    onRenamed();
  };
  const done = () => { if (!quietBusy) closeRename(); };
  const status = adapterStatus(adapter);
  const platform = s?.platform ?? null;
  const setPlatform = async (p: HostPlatform) => {
    setPlatformError(null);
    const result = await runQuiet({ type: "adapter.platform", adapterId: adapter.id, platform: p });
    if (!result.ok) setPlatformError(result.message);
  };
  const connect = async () => {
    setConnectError(null);
    const result = await runQuiet({ type: "adapter.connect", adapterId: adapter.id });
    if (!result.ok) setConnectError(result.message);
  };
  const devices = state.devices.filter((d) => d.adapterId === adapter.id);
  const threshold = state.preferences.lowBatteryPercent;

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
      bar={
        <>
          {busy || quietBusy || adapter.connection === "connecting" ? <Spinner /> : null}
          <button disabled={!connected || !s?.ready} onClick={() => setEditing(true)}>
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
      {!connected && (adapter.connectError ?? connectError) ? <Banner kind="error">{adapter.connectError ?? connectError}</Banner> : null}
      {!connected ? (
        <div className="page-empty">
          <AdapterIcon size={64} />
          <h2>{status.text}</h2>
        </div>
      ) : null}
      {adapter.attention.map((a) => (
        <Banner key={a} kind="warning">
          {a}
        </Banner>
      ))}

      {connected && adapter.readiness === "ready" && platform ? (
        <Card>
          <div className="row">
            <div className="row-text">
              <div className="row-title">Platform</div>
              {platformError ? <div className="row-subtitle">{platformError}</div> : null}
            </div>
            <div className="row-end">
              <Segmented
                label="Platform"
                options={Object.entries(PLATFORMS) as [HostPlatform, string][]}
                value={platform}
                disabled={quietBusy || !s?.ready}
                onChange={(p) => void setPlatform(p)}
              />
            </div>
          </div>
          {platform !== state.hostPlatform ? (
            <Banner action={<button disabled={quietBusy || !s?.ready} onClick={() => void setPlatform(state.hostPlatform)}><SwapIcon /> Switch to {PLATFORMS[state.hostPlatform]}</button>}>
              This computer runs {PLATFORMS[state.hostPlatform]}.
            </Banner>
          ) : null}
        </Card>
      ) : null}

      {devices.length ? (
        <Card title="Devices">
          {devices.map((d) => {
            const low = isLow(d.battery, threshold);
            return (
              <button key={d.key} className="row clickable" onClick={() => onSelect({ page: "device", key: d.key })}>
                <span className={d.device.state === "connected" ? "row-icon on" : "row-icon"}>
                  <DeviceIcon kind={d.kind} />
                </span>
                <span className="row-text">
                  <span className="row-title strong">{d.name}</span>
                  <span className="row-subtitle">{deviceStatus(d.device)}</span>
                </span>
                {d.battery?.percent != null ? <span className={`value${low ? " low" : ""}${batteryStale(d.battery) ? " dim" : ""}`}>{d.battery.percent}%</span> : null}
                <ChevronIcon />
              </button>
            );
          })}
        </Card>
      ) : null}

      {connected && s && s.transports.some((t) => t.maxEnabled !== null) ? (
        <Card title="Active Devices">
          {s.transports.map(({ transport, maxEnabled }) => {
            if (maxEnabled === null) return null;
            const used = devices.filter((d) => d.device.transport === transport && d.device.inactive === null).length;
            return (
              <div key={transport} className="row meter-row">
                <div className="meter-line">
                  <span>{TRANSPORTS[transport]}</span>
                  <span className="value">
                    {used} of {maxEnabled}
                  </span>
                </div>
                <Meter fraction={maxEnabled ? used / maxEnabled : 0} />
              </div>
            );
          })}
        </Card>
      ) : null}

      {connected && s && s.transports.length ? (
        <Card title="New Pairings">
          <Facts>
            {s.transports.map(({ transport }) => (
              <Fact key={transport} label={TRANSPORTS[transport]}>
                {storageFull(s) ? STORAGE_FULL : "Available"}
              </Fact>
            ))}
          </Facts>
        </Card>
      ) : null}

      {connected && s ? (
        <Card title="About">
          <Facts>
            <Fact label="Firmware">
              {String(infoOf(s.info, "firmware.version") ?? "Unknown")} ({infoOf(s.info, "build.development") === true ? "Development" : "Production"})
            </Fact>
            {infoOf(s.info, "board.name") !== undefined ? <Fact label="Board">{String(infoOf(s.info, "board.name"))}</Fact> : null}
            <Fact label="Adapter ID">{s.id}</Fact>
          </Facts>
        </Card>
      ) : null}
    </Page>
  );
}
