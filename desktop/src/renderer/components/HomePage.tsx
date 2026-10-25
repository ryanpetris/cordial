import { isLow } from "../../shared/battery.ts";
import type { AppState } from "../../shared/state.ts";
import { adapterStatus, batteryStale, connected as isConnected } from "../../shared/text.ts";
import type { Selection } from "../App.tsx";
import { Card, Meter, Page, Summary } from "./common.tsx";
import { AdapterIcon, BatteryGlyph, ChevronIcon, DeviceIcon, HomeIcon } from "./icons.tsx";

export function HomePage({ state, onSelect }: { state: AppState; onSelect: (s: Selection) => void }) {
  if (state.adapters.length === 0) {
    return (
      <Page icon={<HomeIcon size={20} />} title="Overview">
        <div className="page-empty">
          <AdapterIcon size={64} />
          <h2>No Adapter Found</h2>
        </div>
      </Page>
    );
  }

  const threshold = state.preferences.lowBatteryPercent;
  const adapterName = (id: string) => state.adapters.find((a) => a.id === id)?.name ?? "";
  const several = state.adapters.filter((a) => a.connection === "connected" || a.connection === "reconnecting").length > 1;
  const connected = state.devices.filter(isConnected);
  // Devices last reported connected keep their tiles while their adapter reconnects.
  const tiles = state.devices.filter((d) => !d.readError && d.device.state === "connected");
  const low = state.devices.filter((d) => isLow(d.battery, threshold));
  return (
    <Page icon={<HomeIcon size={20} />} title="Overview">
      <Summary
        items={[
          ["Connected", connected.length],
          ["Paired", state.devices.length],
          ["Adapters", state.adapters.filter((a) => a.connection === "connected").length],
        ]}
      />

      {low.length ? (
        <Card title="Needs Attention">
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

      {tiles.length ? (
        <section className="group">
          <h2>Devices</h2>
          <div className="tiles">
            {tiles.map((d) => {
              const percent = d.battery?.percent;
              const dim = isLow(d.battery, threshold);
              return (
                <button key={d.key} className={d.available ? "card tile" : "card tile off"} onClick={() => onSelect({ page: "device", key: d.key })}>
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
          const paired = devices.length;
          const on = devices.filter(isConnected).length;
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
