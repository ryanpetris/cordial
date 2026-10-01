import { useState } from "react";
import type { AppState, Preferences as Prefs } from "../../shared/state.ts";
import { act, api } from "../api.ts";
import { Card, Dialog, SwitchRow } from "./common.tsx";
import { CloseIcon } from "./icons.tsx";

export function Preferences({ state, open, onClose }: { state: AppState; open: boolean; onClose: () => void }) {
  const p = state.preferences;
  const set = (preferences: Partial<Prefs>) => void act({ type: "preferences", preferences });
  // The level as typed; it saves once it is a valid level and reverts on blur.
  const [level, setLevel] = useState<string | null>(null);
  const levelValid = (n: number) => Number.isInteger(n) && n >= 5 && n <= 50;
  return (
    <Dialog open={open} title="Preferences" onClose={onClose} className="preferences">
      <div className="dialog-scroll">
        {api.host.desktop ? (
          <Card title="General">
            <SwitchRow title="Start at Login" checked={p.startAtLogin} onChange={(v) => set({ startAtLogin: v })} />
            <SwitchRow title="Always Show Tray Icon" checked={p.alwaysShowTray} onChange={(v) => set({ alwaysShowTray: v })} />
          </Card>
        ) : null}
        <Card title="Notifications">
          <SwitchRow title="Low Battery" checked={p.notifyLowBattery} onChange={(v) => set({ notifyLowBattery: v })} />
          <div className="row">
            <div className="row-text">
              <div className="row-title">Low Battery Level</div>
            </div>
            <div className="row-end">
              <input
                className="number"
                type="number"
                aria-label="Low battery level in percent"
                min={5}
                max={50}
                step={5}
                value={level ?? p.lowBatteryPercent}
                aria-invalid={level !== null && !levelValid(Number(level))}
                onChange={(e) => {
                  setLevel(e.target.value);
                  const n = Number(e.target.value);
                  if (e.target.value !== "" && levelValid(n)) set({ lowBatteryPercent: n });
                }}
                onBlur={() => setLevel(null)}
              />
              <span className="unit">%</span>
            </div>
          </div>
          <SwitchRow title="Connections" checked={p.notifyConnections} onChange={(v) => set({ notifyConnections: v })} />
        </Card>
      </div>
      <footer className="dialog-footer">
        <button className="suggested" onClick={onClose}>
          <CloseIcon /> Close
        </button>
      </footer>
    </Dialog>
  );
}
