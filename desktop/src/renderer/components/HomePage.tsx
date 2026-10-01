import { useState } from "react";
import { isLow } from "../../shared/battery.ts";
import type { AppState } from "../../shared/state.ts";
import { adapterStatus, batteryStale } from "../../shared/text.ts";
import { act, api, chooseAdapter } from "../api.ts";
import type { Selection } from "../App.tsx";
import { Card, Meter, Page, Summary } from "./common.tsx";
import { AdapterIcon, BatteryGlyph, ChevronIcon, DeviceIcon, HomeIcon, PlusIcon, RefreshIcon, WarningIcon } from "./icons.tsx";

export function HomePage({ state, onSelect, onAdd }: { state: AppState; onSelect: (s: Selection) => void; onAdd: () => void }) {
  const [chooseError, setChooseError] = useState<string | null>(null);
  if (state.adapters.length === 0) {
    const choose = api.host.choosePort;
    const pick = async () => {
      setChooseError(null);
      const result = await chooseAdapter();
      if (!result.ok) setChooseError(result.message);
    };
    return (
      <Page icon={<HomeIcon size={20} />} title="Overview">
        <div className="page-empty">
          <AdapterIcon size={64} />
          <h2>No Adapter Found</h2>
          {choose ? (
            <button className="suggested" onClick={() => void pick()}>
              <PlusIcon /> Choose Adapter…
            </button>
          ) : (
            <button onClick={() => void act({ type: "adapters.refresh" })}>
              <RefreshIcon /> Refresh
            </button>
          )}
          {chooseError ? <p className="error-text">{chooseError}</p> : null}
        </div>
      </Page>
    );
  }

  const threshold = state.preferences.lowBatteryPercent;
  const adapterName = (id: string) => state.adapters.find((a) => a.id === id)?.name ?? "";
  const several = state.adapters.filter((a) => a.connection === "connected").length > 1;
  const connected = state.devices.filter((d) => d.device.state === "connected");
  const unpaired = state.devices.filter((d) => d.device.pairing_state === "needs_pairing");
  const low = state.devices.filter((d) => isLow(d.battery, threshold));
  return (
    <Page icon={<HomeIcon size={20} />} title="Overview">
      <Summary
        items={[
          ["Connected", connected.length],
          ["Paired", state.devices.filter((d) => d.device.pairing_state === "paired").length],
          ["Adapters", state.adapters.filter((a) => a.connection === "connected").length],
        ]}
      />

      {unpaired.length || low.length ? (
        <Card title="Needs Attention">
          {unpaired.map((d) => (
            <div key={d.key} className="row">
              <span className="row-icon warn">
                <WarningIcon size={22} />
              </span>
              <div className="row-text">
                <div className="row-title strong">{d.name}</div>
                <div className="row-subtitle">
                  Needs Pairing Again{several ? ` · ${adapterName(d.adapterId)}` : null}
                </div>
              </div>
              <div className="row-end">
                <button onClick={onAdd}>Pair…</button>
              </div>
            </div>
          ))}
          {low.map((d) => (
            <button key={d.key} className="row clickable" onClick={() => onSelect({ page: "device", key: d.key })}>
              <span className="row-icon low">
                <BatteryGlyph percent={d.battery!.percent} charging={d.battery!.charging} low />
              </span>
              <span className="row-text">
                <span className="row-title strong">{d.name}</span>
                <span className="row-subtitle">Battery {d.battery!.percent}%</span>
              </span>
              <ChevronIcon />
            </button>
          ))}
        </Card>
      ) : null}

      {connected.length ? (
        <section className="group">
          <h2>Connected</h2>
          <div className="tiles">
            {connected.map((d) => {
              const percent = d.battery?.percent;
              const dim = isLow(d.battery, threshold);
              return (
                <button key={d.key} className="card tile" onClick={() => onSelect({ page: "device", key: d.key })}>
                  <span className="tile-icon">
                    <DeviceIcon kind={d.kind} size={26} />
                  </span>
                  <span className="tile-text">
                    <span className="tile-line">
                      <span className="row-title strong">{d.name}</span>
                      {percent != null ? (
                        <span className={`tile-percent${dim ? " low" : ""}${batteryStale(d.battery!) ? " dim" : ""}`}>{percent}%</span>
                      ) : null}
                    </span>
                    {percent != null ? <Meter fraction={percent / 100} low={dim} /> : null}
                    {several ? <span className="row-subtitle">{adapterName(d.adapterId)}</span> : null}
                  </span>
                </button>
              );
            })}
          </div>
        </section>
      ) : null}

      <Card title="Adapters">
        {state.adapters.map((a) => {
          const devices = state.devices.filter((d) => d.adapterId === a.id);
          const paired = devices.filter((d) => d.device.pairing_state === "paired").length;
          const on = devices.filter((d) => d.device.state === "connected").length;
          const status = adapterStatus(a);
          return (
            <button key={a.id} className="row clickable" onClick={() => onSelect({ page: "adapter", id: a.id })}>
              <span className={a.connection === "connected" ? "row-icon on" : "row-icon"}>
                <AdapterIcon />
              </span>
              <span className="row-text">
                <span className="row-title strong">{a.name}</span>
                <span className={status.warn ? "row-subtitle warn" : "row-subtitle"}>{status.text}</span>
              </span>
              {a.connection === "connected" ? (
                <span className="value">
                  {paired} paired · {on} connected
                </span>
              ) : null}
              <ChevronIcon />
            </button>
          );
        })}
      </Card>
    </Page>
  );
}
