import { useEffect, useId, useRef, useState } from "react";
import { canEnable, copyName, interfaceName, profileName, profileText } from "../../shared/profiles.ts";
import type { AdapterEntry, AdapterStatus, DeviceEntry, InterfaceChange, InterfaceState, Profile, RoleName } from "../../shared/state.ts";
import { READ_FAILED, ROLES, memoryPercent } from "../../shared/text.ts";
import { act, useAction } from "../api.ts";
import { Card, Dialog, Menu, Meter, Row, Spinner, Staged, Switch } from "./common.tsx";
import {
  ArrowDownIcon,
  ArrowUpIcon,
  CheckIcon,
  CloseIcon,
  KeyboardIcon,
  MediaIcon,
  MoreIcon,
  MouseIcon,
  PlusIcon,
  PowerIcon,
  RefreshIcon,
  TrashIcon,
} from "./icons.tsx";

/** Shown before saving changes that reconnect the adapter's USB. */
export const RECONNECT_TITLE = "USB Reconnect Required";
export const RECONNECT_TEXT = "The adapter will disconnect from this computer for a moment after it saves these changes.";

const ROLE_ICONS: Record<RoleName, (props: { size?: number }) => React.ReactNode> = {
  keyboard: KeyboardIcon,
  mouse: MouseIcon,
  consumer_control: MediaIcon,
  system_control: PowerIcon,
};

/** An icon for each kind of input a profile changes, each named for assistive technology. */
export function RoleIcons({ roles }: { roles: RoleName[] }) {
  if (!roles.length) return null;
  return (
    <span className="role-icons">
      {roles.map((r) => {
        const Icon = ROLE_ICONS[r];
        return (
          <span key={r} role="img" aria-label={ROLES[r]} title={ROLES[r]}>
            <Icon size={18} />
          </span>
        );
      })}
    </span>
  );
}

/** A profile's role icons, when the adapter has named it. */
const rolesOf = (adapter: AdapterEntry, id: number) => adapter.profileNames[id]?.roles ?? [];

/** Previous and Next for the Profiles list's page, or the picker's with `picker`; nothing when
 * every profile fits on one page. */
function PageControls({ adapter, picker, disabled }: { adapter: AdapterEntry; picker: boolean; disabled: boolean }) {
  const [busy, run] = useAction(true);
  const page = picker ? adapter.pickerPage : adapter.profilePage;
  if (!page || (!page.previous && !page.next)) return null;
  const go = (to: "previous" | "next") => void run({ type: "profiles.page", adapterId: adapter.id, page: to, picker });
  return (
    <>
      <button disabled={disabled || busy || page.loading || !page.previous} onClick={() => go("previous")}>
        Previous
      </button>
      <button disabled={disabled || busy || page.loading || !page.next} onClick={() => go("next")}>
        Next
      </button>
    </>
  );
}

/** The name dialog's contents; mounted each time it opens, so it starts from `initial`. */
function NameForm({ initial, label, busy, available, error, onSubmit, onCancel }: {
  initial: string;
  label: string;
  busy: boolean;
  /** Whether the adapter can take the change now. */
  available: boolean;
  error: string | null;
  onSubmit: (name: string) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState(initial);
  const valid = profileName(name) !== null;
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.select(), []);
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (valid && available && !busy) onSubmit(name);
      }}
    >
      <input ref={input} className="name-input" aria-label="Profile name" value={name} disabled={busy} maxLength={64} onChange={(e) => setName(e.target.value)} />
      {error ? <p className="dialog-body error-text">{error}</p> : null}
      <footer className="dialog-footer">
        <button type="button" disabled={busy} onClick={onCancel}>
          <CloseIcon /> Cancel
        </button>
        <button type="submit" className="suggested" disabled={busy || !available || !valid}>
          <CheckIcon /> {label}
        </button>
      </footer>
    </form>
  );
}

type Open = { kind: "create" } | { kind: "copy"; profile: Profile } | { kind: "delete"; profile: Profile };

/** The adapter's profiles a page at a time, each with its roles. Creating, copying and deleting a
 * profile take effect at once. */
export function AdapterProfiles({ adapter, locked }: { adapter: AdapterEntry; locked: boolean }) {
  const [busy, run] = useAction(true);
  const [failure, setFailure] = useState<string | null>(null);
  const [open, setOpen] = useState<Open | null>(null);
  const [menu, setMenu] = useState<{ profile: Profile; x: number; y: number } | null>(null);
  const opener = useRef<HTMLButtonElement | null>(null);
  const ready = adapter.connection === "connected" && !!adapter.status?.ready;
  const page = adapter.profilePage;
  const profiles = page?.profiles ?? [];
  const disabled = busy || locked || !ready;
  const close = () => {
    if (busy) return;
    setOpen(null);
    setFailure(null);
  };
  const inDialog = async (action: Parameters<typeof run>[0]) => {
    setFailure(null);
    const result = await run(action);
    if (result.ok) setOpen(null);
    else setFailure(result.message);
  };
  const dialogTitle = open?.kind === "create" ? "New Profile"
    : open?.kind === "copy" ? `Copy “${open.profile.name}”`
      : open?.kind === "delete" ? `Delete “${open.profile.name}”?` : "";
  const rows = [
    ...profiles.map((p) => (
      <Row key={p.id} title={p.name}>
        <RoleIcons roles={p.roles} />
        <button
          className="icon-button"
          aria-label={`${p.name}, Options`}
          aria-haspopup="menu"
          aria-expanded={menu?.profile.id === p.id}
          disabled={disabled}
          onClick={(e) => {
            opener.current = e.currentTarget;
            const r = e.currentTarget.getBoundingClientRect();
            setMenu(menu?.profile.id === p.id ? null : { profile: p, x: r.right, y: r.bottom });
          }}
        >
          <MoreIcon />
        </button>
      </Row>
    )),
    ...(page?.unreadable ?? []).map((id) => <Row key={id} title={`Profile ${id}`} subtitle="Couldn't Read" dim />),
  ];
  return (
    <>
      <Card
        title="Profiles"
        footer={
          <>
            {busy || page?.loading ? <Spinner /> : null}
            <PageControls adapter={adapter} picker={false} disabled={!ready} />
            <button className={rows.length || page?.previous ? undefined : "suggested"} disabled={disabled || !page} onClick={() => setOpen({ kind: "create" })}>
              <PlusIcon /> New Profile
            </button>
          </>
        }
      >
        {page?.error ? (
          <Row title={page.error === READ_FAILED ? "The adapter couldn't read its profiles. Try again." : `The adapter couldn't read its profiles. ${page.error}`}>
            <button disabled={disabled} onClick={() => void run({ type: "adapter.reload", adapterId: adapter.id })}>
              <RefreshIcon /> Retry
            </button>
          </Row>
        ) : null}
        {rows.length ? rows : null}
      </Card>

      {menu ? (
        <Menu
          label={`${menu.profile.name}, Options`}
          x={menu.x}
          y={menu.y}
          options={[
            ["Copy", () => setOpen({ kind: "copy", profile: menu.profile })],
            ["Delete", () => setOpen({ kind: "delete", profile: menu.profile })],
          ]}
          onClose={(refocus) => {
            setMenu(null);
            if (refocus) opener.current?.focus();
          }}
        />
      ) : null}

      <Dialog open={!!open} title={dialogTitle} onClose={close}>
        {open?.kind === "create" || open?.kind === "copy" ? (
          <NameForm
            initial={open.kind === "create" ? "" : copyName(open.profile.name)}
            label={open.kind === "create" ? "Create" : "Copy"}
            busy={busy}
            available={ready}
            error={failure}
            onCancel={close}
            onSubmit={(name) =>
              void inDialog(open.kind === "create"
                ? { type: "profile.create", adapterId: adapter.id, name }
                : { type: "profile.copy", adapterId: adapter.id, profile: open.profile.id, name })}
          />
        ) : open ? (
          <>
            {failure ? <p className="dialog-body error-text">{failure}</p> : null}
            <footer className="dialog-footer">
              <button disabled={busy} onClick={close}>
                <CloseIcon /> Cancel
              </button>
              <button className="destructive" disabled={busy || !ready} onClick={() => void inDialog({ type: "profile.delete", adapterId: adapter.id, profile: open.profile.id })}>
                <TrashIcon /> Delete
              </button>
            </footer>
          </>
        ) : null}
      </Dialog>
    </>
  );
}

/** How much of the adapter's profile memory loaded profiles use. */
export function ProfileMemory({ status }: { status: AdapterStatus }) {
  const percent = memoryPercent(status);
  if (percent === null) return null;
  return (
    <Card title="Profile Memory">
      <div className="row meter-row">
        <div className="meter-line">
          <span>In Use</span>
          <span className="value">{percent}%</span>
        </div>
        <Meter fraction={percent / 100} low={percent >= 85} />
      </div>
    </Card>
  );
}

/** Chooses a profile from the adapter's pages, starting at the first; mounted each time it opens.
 * With `none`, the choice can also be no profile, which is 0. */
function ProfilePicker({ adapter, value, none, label, onChoose, onCancel }: {
  adapter: AdapterEntry;
  value: number;
  none: boolean;
  label: string;
  onChoose: (profile: number) => void;
  onCancel: () => void;
}) {
  const [chosen, setChosen] = useState(value);
  const group = useId();
  const page = adapter.pickerPage;
  const adapterId = adapter.id;
  useEffect(() => void act({ type: "profiles.page", adapterId, page: "first", picker: true }, true), [adapterId]);
  const choices: [number, string, RoleName[]][] = [
    ...(none ? [[0, "None", []] as [number, string, RoleName[]]] : []),
    ...(page?.profiles ?? []).map((p): [number, string, RoleName[]] => [p.id, p.name, p.roles]),
  ];
  // The current choice stays listed when it is on another page.
  if (value && !choices.some(([id]) => id === value)) choices.push([value, profileText(adapter, value), rolesOf(adapter, value)]);
  return (
    <>
      <div className="dialog-scroll">
        <div className="card" role="radiogroup" aria-label={label}>
          {choices.map(([id, text, roles]) => (
            <label key={id} className="row clickable">
              <span className="row-text">{text}</span>
              <span className="row-end">
                <RoleIcons roles={roles} />
                <input type="radio" className="radio" name={group} aria-label={text} checked={chosen === id} onChange={() => setChosen(id)} />
              </span>
            </label>
          ))}
        </div>
        {page?.error ? <p className="dialog-body error-text">{page.error}</p> : null}
      </div>
      <footer className="dialog-footer">
        {page?.loading ? <Spinner /> : null}
        <PageControls adapter={adapter} picker disabled={false} />
        <button onClick={onCancel}>
          <CloseIcon /> Cancel
        </button>
        <button className="suggested" disabled={!none && !chosen} onClick={() => onChoose(chosen)}>
          <CheckIcon /> Choose
        </button>
      </footer>
    </>
  );
}

/** Each configuration interface with its own switch and profile, staged until Save. An interface
 * can be turned on only with a profile and while no interface it conflicts with is on, and an
 * enabled one keeps a profile. */
export function ConfigurationInterfaces({ adapter, interfaces, changed, locked, onChange }: {
  adapter: AdapterEntry;
  /** The interfaces with staged values in place of saved ones. */
  interfaces: InterfaceState[];
  /** The staged changes that differ from the saved values. */
  changed: Record<string, InterfaceChange>;
  locked: boolean;
  onChange: (i: number, change: InterfaceChange) => void;
}) {
  const [picking, setPicking] = useState<InterfaceState | null>(null);
  const shown = interfaces.filter((i) => interfaceName(i.interface));
  if (!shown.length) return null;
  return (
    <Card title="Configuration Interfaces">
      {shown.map((i) => {
        const name = interfaceName(i.interface)!;
        const staged = changed[i.interface];
        return (
          <Row key={i.interface} title={name}>
            {staged ? <Staged label={name} /> : null}
            <button aria-label={`${name} Profile`} aria-haspopup="dialog" disabled={locked} onClick={() => setPicking(i)}>
              {i.profile ? profileText(adapter, i.profile) : "Choose Profile"}
            </button>
            <Switch label={name} checked={i.enabled} disabled={locked || (!i.enabled && !canEnable(interfaces, i))} onChange={(enabled) => onChange(i.interface, { enabled })} />
          </Row>
        );
      })}
      <Dialog open={!!picking} title={picking ? `${interfaceName(picking.interface)} Profile` : ""} onClose={() => setPicking(null)}>
        {picking ? (
          <ProfilePicker
            adapter={adapter}
            value={picking.profile}
            none={!picking.enabled}
            label={`${interfaceName(picking.interface)} Profile`}
            onCancel={() => setPicking(null)}
            onChoose={(profile) => {
              onChange(picking.interface, { profile });
              setPicking(null);
            }}
          />
        ) : null}
      </Dialog>
    </Card>
  );
}

/** A device's layers: the profiles applied to its input, in order. Changes stage until Save. */
export function DeviceLayers({ entry, adapter, layers, changed, locked, onChange }: {
  entry: DeviceEntry;
  adapter: AdapterEntry | undefined;
  /** The staged layers, else the saved ones. */
  layers: number[];
  changed: boolean;
  locked: boolean;
  onChange: (layers: number[]) => void;
}) {
  const [adding, setAdding] = useState(false);
  const support = adapter?.status?.profileSupport;
  if (!adapter || !support || entry.device.profiles === null) return null;
  const move = (from: number, to: number) => {
    const next = [...layers];
    next.splice(to, 0, ...next.splice(from, 1));
    onChange(next);
  };
  return (
    <section className="group">
      <h2>
        Profiles
        {changed ? <Staged label="Profiles" /> : null}
      </h2>
      {layers.length ? (
        <div className="card">
          {layers.map((id, n) => {
            const name = profileText(adapter, id);
            return (
              <Row key={`${n}-${id}`} title={name}>
                <RoleIcons roles={rolesOf(adapter, id)} />
                <button className="icon-button" aria-label={`${name}, Move Up`} disabled={locked || n === 0} onClick={() => move(n, n - 1)}>
                  <ArrowUpIcon />
                </button>
                <button className="icon-button" aria-label={`${name}, Move Down`} disabled={locked || n === layers.length - 1} onClick={() => move(n, n + 1)}>
                  <ArrowDownIcon />
                </button>
                <button className="icon-button" aria-label={`${name}, Remove`} disabled={locked} onClick={() => onChange(layers.filter((_, i) => i !== n))}>
                  <CloseIcon />
                </button>
              </Row>
            );
          })}
        </div>
      ) : null}
      <div className="group-footer">
        <button disabled={locked || layers.length >= support.maxLayers} onClick={() => setAdding(true)}>
          <PlusIcon /> Add Profile
        </button>
      </div>
      <Dialog open={adding} title="Add Profile" onClose={() => setAdding(false)}>
        {adding ? (
          <ProfilePicker
            adapter={adapter}
            value={0}
            none={false}
            label="Profile"
            onCancel={() => setAdding(false)}
            onChoose={(profile) => {
              onChange([...layers, profile]);
              setAdding(false);
            }}
          />
        ) : null}
      </Dialog>
    </section>
  );
}
