import { useEffect, useRef, useState } from "react";
import type { AdapterEntry, AppState, DeviceEntry } from "../../shared/state.ts";
import { adapterStatus, batteryStale, batteryText, deviceStatus } from "../../shared/text.ts";
import { act, api, chooseAdapter, reportError } from "../api.ts";
import type { Selection } from "../App.tsx";
import { AdapterIcon, AppIcon, BatteryGlyph, DeviceIcon, GearIcon, HomeIcon, MenuIcon, PlugIcon, PlusIcon, RefreshIcon } from "./icons.tsx";
import { isLow } from "../../shared/battery.ts";

function DeviceRow({ d, selected, threshold, onSelect }: { d: DeviceEntry; selected: boolean; threshold: number; onSelect: () => void }) {
  const low = isLow(d.battery, threshold);
  return (
    <button className={selected ? "side-row selected" : "side-row"} aria-current={selected} onClick={onSelect}>
      <span className={d.device.state === "connected" ? "side-icon on" : "side-icon"}>
        <DeviceIcon kind={d.kind} />
      </span>
      <span className="side-text">
        <span className="side-title">{d.name}</span>
        <span className="side-subtitle">{deviceStatus(d.device)}</span>
      </span>
      {d.battery ? (
        <span className={`side-battery${low ? " low" : ""}${batteryStale(d.battery) ? " dim" : ""}`} title={`Battery ${batteryText(d.battery) ?? "unknown"}`}>
          {d.battery.percent != null ? `${d.battery.percent}%` : null}
          <BatteryGlyph percent={d.battery.percent} charging={d.battery.charging} low={low} />
        </span>
      ) : null}
    </button>
  );
}

/** A placeholder row that takes the next step for an empty section. */
function ActionRow({ icon, label, title, onClick }: { icon: React.ReactNode; label: string; title?: string; onClick: () => void }) {
  return (
    <button className="side-row action" title={title} onClick={onClick}>
      <span className="side-icon">{icon}</span>
      <span className="side-title">{label}</span>
    </button>
  );
}

/** The adapter menu for hosts without native menus, opened at the pointer. */
function AdapterMenu({ adapter, x, y, onRename, onClose }: { adapter: AdapterEntry; x: number; y: number; onRename: () => void; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  // A manual popover: on Linux the menu opens on button press, and automatic
  // light dismissal would close it again on release.
  useEffect(() => {
    const menu = ref.current!;
    menu.showPopover();
    menu.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
    const outside = (e: PointerEvent) => !menu.contains(e.target as Node) && onClose();
    const escape = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    window.addEventListener("blur", onClose);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
      window.removeEventListener("blur", onClose);
      menu.hidePopover();
    };
  }, []);
  const connected = adapter.connection === "connected";
  const run = (action: () => void) => () => {
    onClose();
    action();
  };
  return (
    <div
      ref={ref}
      popover="manual"
      role="menu"
      className="context-menu"
      style={{ left: Math.min(x, window.innerWidth - 200), top: Math.min(y, window.innerHeight - 90) }}
    >
      {connected ? (
        <button role="menuitem" onClick={run(() => void act({ type: "adapter.disconnect", adapterId: adapter.id }))}>
          Disconnect
        </button>
      ) : (
        <button role="menuitem" disabled={adapter.connection !== "disconnected"} onClick={run(() => void act({ type: "adapter.connect", adapterId: adapter.id }))}>
          Connect
        </button>
      )}
      <button role="menuitem" disabled={!connected || !adapter.status?.ready} onClick={run(onRename)}>
        Rename
      </button>
    </div>
  );
}

export function Sidebar({
  state,
  selection,
  onSelect,
  onAdd,
  onPreferences,
  onRename,
}: {
  state: AppState;
  selection: Selection;
  onSelect: (s: Selection) => void;
  onAdd: () => void;
  onPreferences: () => void;
  onRename: (adapterId: string) => void;
}) {
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const menuAdapter = menu && state.adapters.find((a) => a.id === menu.id);
  const choose = api.host.choosePort;
  const connected = state.adapters.filter((a) => a.connection === "connected");
  const threshold = state.preferences.lowBatteryPercent;
  const canAdd = connected.some((a) => a.readiness === "ready");
  // Without a ready adapter there is nothing to add to, so offer to connect the ones that can be.
  const connectable = canAdd ? [] : state.adapters.filter((a) => a.connection === "disconnected");
  const pick = () => void chooseAdapter().then((result) => { if (!result.ok) reportError(result.message); });
  return (
    <nav className="sidebar" aria-label="Devices and adapters">
      <header className="sidebar-header">
        <span className="app-title">
          <AppIcon /> Cordial
        </span>
        {api.host.desktop ? (
          <button
            className="icon-button"
            title="Main Menu"
            aria-label="Main Menu"
            aria-haspopup="menu"
            onClick={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              void act({ type: "app.menu", x: r.left, y: r.bottom });
            }}
          >
            <MenuIcon />
          </button>
        ) : (
          <button className="icon-button" title="Settings" aria-label="Settings" onClick={onPreferences}>
            <GearIcon />
          </button>
        )}
      </header>
      <div className="sidebar-scroll">
        <button className={selection.page === "home" ? "side-row compact selected" : "side-row compact"} aria-current={selection.page === "home"} onClick={() => onSelect({ page: "home" })}>
          <span className="side-icon">
            <HomeIcon />
          </span>
          <span className="side-title">Overview</span>
        </button>
        {state.devices.length || canAdd || connectable.length ? <h3 className="side-heading">Devices</h3> : null}
        {state.devices.map((d) => (
          <DeviceRow
            key={d.key}
            d={d}
            threshold={threshold}
            selected={selection.page === "device" && selection.key === d.key}
            onSelect={() => onSelect({ page: "device", key: d.key })}
          />
        ))}
        {canAdd && state.devices.length === 0 ? <ActionRow icon={<PlusIcon size={24} />} label="Add Device" onClick={onAdd} /> : null}
        {connectable.map((a) => (
          <ActionRow key={a.id} icon={<PlugIcon size={24} />} label={`Connect ${a.name}`} onClick={() => void act({ type: "adapter.connect", adapterId: a.id })} />
        ))}
        <h3 className="side-heading">
          Adapters
          {choose && state.adapters.length ? (
            <button className="icon-button" title="Choose Adapter" aria-label="Choose Adapter" onClick={pick}>
              <PlusIcon />
            </button>
          ) : null}
        </h3>
        {state.adapters.map((a) => {
          const selected = selection.page === "adapter" && selection.id === a.id;
          const status = adapterStatus(a);
          return (
            <button
              key={a.id}
              className={`${selected ? "side-row selected" : "side-row"}${a.connection !== "connected" ? " off" : ""}`}
              aria-current={selected}
              onClick={() => onSelect({ page: "adapter", id: a.id })}
              onContextMenu={(e) => {
                e.preventDefault();
                if (api.host.desktop) void act({ type: "adapter.menu", adapterId: a.id });
                else setMenu({ id: a.id, x: e.clientX, y: e.clientY });
              }}
            >
              <span className={a.connection === "connected" ? "side-icon on" : "side-icon"}>
                <AdapterIcon />
              </span>
              <span className="side-text">
                <span className="side-title">{a.name}</span>
                <span className={status.warn ? "side-subtitle warn" : "side-subtitle"}>{status.text}</span>
              </span>
            </button>
          );
        })}
        {state.adapters.length === 0 ? (
          choose ? (
            <ActionRow icon={<PlusIcon size={24} />} label="Choose Adapter" onClick={pick} />
          ) : (
            <ActionRow icon={<RefreshIcon size={24} />} label="Refresh Adapters" title={api.host.desktop ? "Refresh Adapters (F5)" : undefined} onClick={() => void act({ type: "adapters.refresh" })} />
          )
        ) : null}
        {menuAdapter ? (
          <AdapterMenu key={menu.id} adapter={menuAdapter} x={menu.x} y={menu.y} onRename={() => onRename(menu.id)} onClose={() => setMenu(null)} />
        ) : null}
      </div>
      {state.devices.length ? (
        <footer className="sidebar-footer">
          <button className="suggested wide" onClick={onAdd}>
            <PlusIcon /> Add Device
          </button>
        </footer>
      ) : null}
    </nav>
  );
}
