// One adapter's session: a Connection and the adapter's state, kept current
// from responses and events in the order the adapter sent them. Each device
// record and profile arrives whole, so the latest one wins. Each page of a
// listing replaces what was known in the range of keys it covers, and settings
// and warning events carry only changes, which are merged into what has been
// listed. A successful change command means the adapter saved exactly what was
// sent, so its values are applied here without reading them back. Opening
// reads every page of devices and, when the adapter has profiles, the first
// page of profiles, then each device's warnings and settings. Profiles are
// shown a page at a time; profiles in a device's layers or selected by a
// configuration interface that no page has shown are named with GetProfile.
import { create } from "@bufbuild/protobuf";
import {
  Connection,
  ConnectionClosedError,
  CordialError,
  UnexpectedResponseError,
  compareSettingRefs,
  entryId,
  compareWarnings,
  settingRef,
  type ByteStream,
} from "@cordial/client";
import {
  ErrorCode,
  Platform,
  SettingSchema,
  SettingState,
  type DeviceWarning as WireWarning,
  type Event,
  type Request,
  type Response,
  type Setting as WireSetting,
  type SetAdapter,
  type SetDevice,
  type SetSettings,
} from "@cordial/protocol";
import type { AdapterStatus, DeviceRecord, DeviceWarning, Profile, ProfilePage, Setting } from "../shared/state.ts";
import { READ_FAILED, asSentence, errorText } from "../shared/text.ts";
import * as convert from "./convert.ts";

export interface SessionHooks {
  /** Some adapter state visible to the user changed. */
  changed(): void;
  /** The session ended by itself: unplugged, failed or protocol violation. */
  closed(error: Error): void;
  /** A scan or pairing event, which the controller tracks. */
  event(event: Event): void;
  /** The adapter answered a request; events after this follow its result. */
  answered?(request: Request, response: Response): void;
  log(message: string): void;
}

/** Why a failed request failed, in words; `read` marks a request that only reads saved data. */
export const failure = (error: unknown, read = false) =>
  error instanceof CordialError && read && error.code === ErrorCode.STORAGE_FAILED ? READ_FAILED
  : error instanceof CordialError ? errorText(convert.wireError(error))
    : error instanceof ConnectionClosedError
      ? read ? "The adapter disconnected before it answered." : "The adapter disconnected before the change finished."
      : error instanceof UnexpectedResponseError ? "The adapter returned an unexpected result."
      : asSentence((error as Error).message);

/** Whether `id` is in the range a page read after `after` covers, up to `next` (0 ends the listing). */
const inPage = (id: number, after: number, next: number) => id > after && (next === 0 || id <= next);

/**
 * Replaces the entries of `list` in the range of keys a page covers, after `after` (undefined from
 * the start) up to the page's last entry, or to the end of the listing when `end` is set, with the
 * page's entries, keeping the listing's order.
 */
function mergePage<E, K>(list: E[], page: E[], after: K | undefined, end: boolean, key: (e: E) => K, compare: (a: K, b: K) => number): E[] {
  const last = page.at(-1);
  const hi = end || last === undefined ? undefined : key(last);
  const covered = (e: E) => (after === undefined || compare(key(e), after) > 0) && (hi === undefined || compare(key(e), hi) <= 0);
  return [...list.filter((e) => !covered(e)), ...page].sort((a, b) => compare(key(a), key(b)));
}

/** A setting as the adapter would list it after SetSettings saved or forgot `change`'s value. */
function changedSetting(s: WireSetting, change: SetSettings["changes"][number]): WireSetting {
  const next = create(SettingSchema, s);
  const t = next.type;
  const c = change.change;
  if (c.case === "forget") {
    if (t.case) t.value.saved = undefined;
    next.status = { case: undefined };
    return next;
  }
  const v = c.value?.value;
  if (!v) return next;
  if (t.case === "bool" && v.case === "bool") t.value.saved = v.value;
  else if (t.case === "integer" && v.case === "integer") t.value.saved = v.value;
  else if ((t.case === "enum" || t.case === "text") && v.case === "text") t.value.saved = v.value;
  else if (t.case === "color" && v.case === "color") t.value.saved = v.value;
  else return next;
  next.status = { case: "state", value: SettingState.PENDING };
  return next;
}

/** Which list of profiles a page belongs to: the adapter's Profiles list, or the profile picker,
 * which pages on its own. */
export type PageView = "list" | "picker";

/** The shown page of profiles, with the cursors that led to it. */
interface PageState {
  /** The `after` cursor of each shown page up to the current one, first page first. */
  cursors: number[];
  profiles: Profile[];
  unreadable: number[];
  next: number;
  loading: boolean;
  error: string | null;
  /** Counts reads, so only the latest one's result is shown. */
  read: number;
}

/**
 * The inactive reason a saved enabled or blocked change leaves, in the adapter's order: transport
 * reasons stay, then blocked, then disabled. An accepted enable was given room, so the device is
 * no longer inactive for capacity.
 */
function inactiveAfter(d: DeviceRecord): DeviceRecord["inactive"] {
  if (d.inactive === "unsupported_transport" || d.inactive === "transport_disabled") return d.inactive;
  if (d.blocked) return "blocked";
  if (!d.enabled) return "disabled";
  return d.inactive === "capacity" ? d.inactive : null;
}

export class AdapterSession {
  status: AdapterStatus;
  readonly devices = new Map<number, DeviceRecord>();
  /** What has been listed and reported of each device's settings and warnings, in the adapter's
   * order; `settings` and `warnings` are made from them. */
  readonly #wireSettings = new Map<number, WireSetting[]>();
  readonly #wireWarnings = new Map<number, WireWarning[]>();
  readonly settings = new Map<number, Setting[]>();
  readonly settingsErrors = new Map<number, string>();
  readonly warnings = new Map<number, DeviceWarning[]>();
  readonly warningsErrors = new Map<number, string>();
  /** Every page of devices has been read. */
  listed = false;
  /** Profiles seen this session, by ID. */
  readonly profileNames = new Map<number, Profile>();
  readonly #pages: Record<PageView, PageState | null> = { list: null, picker: null };
  /** IDs being named with GetProfile. */
  readonly #naming = new Set<number>();
  /** IDs the adapter couldn't name this session; asked again only after the profile changes. */
  readonly #unnamed = new Set<number>();

  readonly #hooks: SessionHooks;
  #connection!: Connection;
  /** Commands in flight, per device. */
  readonly #pending = new Map<number, string[]>();
  #syncing: Promise<void> | null = null;
  #syncAgain = false;

  private constructor(hooks: SessionHooks) {
    this.#hooks = hooks;
    this.status = null as unknown as AdapterStatus;
  }

  /** Starts a session and reads the adapter's status; rejects if the port isn't a working adapter. */
  static async open(stream: ByteStream, hooks: SessionHooks): Promise<AdapterSession> {
    const session = new AdapterSession(hooks);
    session.#connection = await Connection.open(stream, {
      onEvent: (event) => session.#event(event),
      onResponse: (request, response) => session.#response(request, response),
      onClose: (error) => hooks.closed(error),
      log: hooks.log,
    });
    // Callbacks during the handshake have already applied the status and any later adapter event.
    session.status ??= convert.status(session.#connection.status);
    return session;
  }

  get adapterId(): string {
    return this.status.id;
  }

  get closed(): boolean {
    return this.#connection.closed;
  }

  get connection(): Connection {
    return this.#connection;
  }

  /** Reads the devices and keeps them current. */
  run() {
    this.#sync();
  }

  /** Sends a command for a device, tracking it as pending while it runs. */
  async perform<T>(device: number, command: string, run: (c: Connection) => Promise<T>): Promise<T> {
    const list = this.#pending.get(device) ?? [];
    list.push(command);
    this.#pending.set(device, list);
    this.#hooks.changed();
    try {
      return await run(this.#connection);
    } finally {
      list.splice(list.indexOf(command), 1);
      if (!list.length) this.#pending.delete(device);
      this.#hooks.changed();
    }
  }

  pendingFor(id: number): string[] {
    return [...(this.#pending.get(id) ?? [])];
  }

  /** Reads every page of devices, then each device's warnings and settings (single flight). */
  #sync() {
    if (this.#syncing) {
      this.#syncAgain = true;
      return;
    }
    this.#syncing = (async () => {
      do {
        this.#syncAgain = false;
        try {
          // Each page's response updates the devices in its range as it arrives.
          await this.#connection.listAllDevices();
          this.listed = true;
          this.#hooks.changed();
          if (this.status.profileSupport) {
            await this.readPage(this.#pages.list ? "again" : "first");
            if (this.#pages.picker) await this.readPage("again", "picker");
            await this.#nameReferences();
          }
          for (const id of [...this.devices.keys()]) await this.#readLists(id);
        } catch (error) {
          if (this.closed) break;
          this.#hooks.log(`listing devices failed: ${failure(error, true)}`);
        }
      } while (this.#syncAgain && !this.closed);
      this.#syncing = null;
    })();
  }

  /** The Profiles list's page; null without profile support or before the first read. */
  get profilePage(): ProfilePage | null {
    return this.#pageOf("list");
  }

  /** The profile picker's page; null without profile support or before its first read. */
  get pickerPage(): ProfilePage | null {
    return this.#pageOf("picker");
  }

  #pageOf(view: PageView): ProfilePage | null {
    const p = this.#pages[view];
    if (!p || !this.status.profileSupport) return null;
    return { profiles: p.profiles, unreadable: p.unreadable, previous: p.cursors.length > 1, next: p.next !== 0, loading: p.loading, error: p.error };
  }

  /** Reads the first, next or previous page of profiles, or the shown one again. The cursors and
   * the shown page change only when the read succeeds. A page that has emptied with an earlier
   * one before it, as after deleting its last profile, shows the earlier one instead. */
  async readPage(page: "first" | "next" | "previous" | "again", view: PageView = "list") {
    if (!this.status.profileSupport || this.closed) return;
    const state = (this.#pages[view] ??= { cursors: [0], profiles: [], unreadable: [], next: 0, loading: false, error: null, read: 0 });
    // The cursors before the page to show, and the cursor that starts reading it.
    let before: number[];
    let after: number;
    if (page === "first") [before, after] = [[], 0];
    else if (page === "next") {
      if (!state.next) return;
      [before, after] = [[...state.cursors], state.next];
    } else if (page === "previous") {
      if (state.cursors.length < 2) return;
      [before, after] = [state.cursors.slice(0, -2), state.cursors.at(-2)!];
    } else [before, after] = [state.cursors.slice(0, -1), state.cursors.at(-1) ?? 0];
    const read = ++state.read;
    state.loading = true;
    this.#hooks.changed();
    try {
      let list = await this.#connection.listProfiles(after);
      while (!list.entries.length && before.length) {
        if (read !== state.read) return;
        after = before.pop()!;
        list = await this.#connection.listProfiles(after);
      }
      if (read !== state.read) return;
      // The cursor of the following page is the key of this page's last entry with a known key.
      const last = list.entries.map(entryId).findLast((id) => id !== undefined);
      if (last === undefined && !list.end) throw new UnexpectedResponseError("listProfiles");
      state.cursors = [...before, after];
      state.profiles = list.entries.flatMap((e) => (e.entry.case === "profile" ? [convert.profile(e.entry.value)] : []));
      state.unreadable = list.entries.flatMap((e) => (e.entry.case === "unreadable" ? [e.entry.value] : []));
      state.next = list.end ? 0 : last!;
      state.error = null;
    } catch (error) {
      if (this.closed || read !== state.read) return;
      state.error = failure(error, true);
      this.#hooks.log(`profiles unavailable: ${state.error}`);
    } finally {
      if (read === state.read) state.loading = false;
      this.#hooks.changed();
    }
  }

  /** Reads the Profiles list's page again, as after a failed read. */
  reloadProfiles() {
    void this.readPage("again");
  }

  /** Names the profiles in devices' layers and interfaces that nothing has named yet. */
  async #nameReferences() {
    if (!this.status.profileSupport || !this.listed || this.closed) return;
    const ids = this.status.interfaces.map((i) => i.profile);
    for (const d of this.devices.values()) ids.push(...(d.profiles ?? []));
    for (const id of new Set(ids)) {
      if (!id || this.profileNames.has(id) || this.#naming.has(id) || this.#unnamed.has(id)) continue;
      this.#naming.add(id);
      try {
        await this.#connection.getProfile(id);
      } catch (error) {
        if (this.closed) return;
        this.#unnamed.add(id);
        this.#hooks.log(`profile ${id} unavailable: ${failure(error, true)}`);
      } finally {
        this.#naming.delete(id);
      }
    }
  }

  async #readLists(id: number) {
    if (!this.devices.has(id) || this.closed) return;
    await this.#connection.listAllWarnings(id).then(
      () => this.warningsErrors.delete(id),
      (error: unknown) => this.#readFailed(id, this.warningsErrors, "warnings", error),
    );
    if (!this.devices.has(id) || this.closed) return;
    await this.#connection.listAllSettings(id).then(
      () => this.settingsErrors.delete(id),
      (error: unknown) => this.#readFailed(id, this.settingsErrors, "settings", error),
    );
    this.#hooks.changed();
  }

  #readFailed(id: number, errors: Map<number, string>, what: string, error: unknown) {
    if (this.closed || !this.devices.has(id)) return;
    const reason = failure(error, true);
    this.#hooks.log(`${what} of ${id} unavailable: ${reason}`);
    errors.set(id, reason);
  }

  /** Reads a device's warning and settings lists again, as after a failed read. */
  reload(id: number) {
    void this.#readLists(id);
  }

  // ---- State --------------------------------------------------------------

  #response(request: Request, response: Response) {
    const result = response.result;
    const command = request.command;
    this.#hooks.answered?.(request, response);
    switch (result.case) {
      case undefined:
        this.#applied(command);
        break;
      case "status":
        this.#setStatus(result.value);
        break;
      case "devices": {
        // The page replaces the devices in its range. A device whose record couldn't be read keeps
        // what was known of it: a failed read is no proof that it is gone.
        const after = command.case === "listDevices" ? command.value.after : 0;
        const { entries, end } = result.value;
        const devices = entries.flatMap((e) => (e.entry.case === "device" ? [e.entry.value] : []));
        const unreadable = entries.flatMap((e) => (e.entry.case === "unreadable" ? [e.entry.value] : []));
        // The page covers up to its last entry with a known key; one without such an entry that
        // doesn't end the listing covers nothing.
        const last = entries.map(entryId).findLast((id) => id !== undefined);
        const listed = new Set(devices.map((d) => d.id));
        if (end || last !== undefined)
          for (const id of [...this.devices.keys()])
            if (inPage(id, after, end ? 0 : last!) && !listed.has(id) && !unreadable.includes(id)) this.#remove(id);
        for (const d of devices) this.devices.set(d.id, convert.device(d));
        if (unreadable.length) this.#hooks.log(`devices ${unreadable.join(", ")} unreadable`);
        break;
      }
      case "device":
        this.#putDevice(convert.device(result.value), false);
        break;
      case "settings": {
        const { device, settings, end } = result.value;
        if (!this.devices.has(device) || command.case !== "listSettings") break;
        const after = command.value.after;
        this.#putSettings(device, mergePage(this.#wireSettings.get(device) ?? [], settings, after, end, settingRef, compareSettingRefs));
        break;
      }
      case "warnings": {
        const { device, warnings, end } = result.value;
        if (!this.devices.has(device) || command.case !== "listWarnings") break;
        const after = command.value.after;
        this.#putWarnings(device, mergePage(this.#wireWarnings.get(device) ?? [], warnings, after, end, (w) => w, compareWarnings));
        break;
      }
      case "profile":
        this.#named(convert.profile(result.value));
        break;
      case "profiles":
        for (const e of result.value.entries) if (e.entry.case === "profile") this.#named(convert.profile(e.entry.value));
        break;
      case "profileCreated":
        if (command.case === "createProfile") this.#profileChanged({ id: result.value.profile, name: command.value.name, roles: [] });
        else if (command.case === "copyProfile")
          this.#profileChanged({ id: result.value.profile, name: command.value.name, roles: [...(this.profileNames.get(command.value.profile)?.roles ?? [])] });
        break;
    }
    this.#hooks.changed();
  }

  /** Applies what a change command that succeeded sent: the adapter now holds exactly that. */
  #applied(command: Request["command"]) {
    switch (command.case) {
      case "setAdapter":
        this.#appliedAdapter(command.value);
        break;
      case "setDevice":
        this.#appliedDevice(command.value);
        break;
      case "setSettings": {
        const { device, changes } = command.value;
        const list = this.#wireSettings.get(device);
        if (!list || !this.devices.has(device)) break;
        const next = list.map((s) => changes.reduce((x, c) => (compareSettingRefs(settingRef(c), settingRef(x)) === 0 ? changedSetting(x, c) : x), s));
        this.#putSettings(device, next);
        break;
      }
    }
  }

  #appliedAdapter(update: SetAdapter) {
    const status = { ...this.status, transports: this.status.transports.map((t) => ({ ...t })), interfaces: this.status.interfaces.map((i) => ({ ...i })) };
    // An empty name restores the firmware's default, which the adapter event that follows names.
    if (update.name) status.name = update.name;
    if (update.platform !== undefined && update.platform in Platform) status.platform = convert.name(Platform, update.platform);
    for (const u of update.transports) {
      const t = status.transports.find((x) => x.transport === convert.transport(u.transport));
      if (t && u.enabled !== undefined) t.enabled = u.enabled;
    }
    for (const u of update.configurationInterfaces) {
      const i = status.interfaces.find((x) => x.interface === u.interface);
      if (!i) continue;
      if (u.enabled !== undefined) i.enabled = u.enabled;
      if (u.profile !== undefined) i.profile = u.profile;
    }
    this.status = status;
  }

  #appliedDevice(update: SetDevice) {
    const d = this.devices.get(update.device);
    if (!d) return;
    const next: DeviceRecord = { ...d };
    if (update.enabled !== undefined) next.enabled = update.enabled;
    if (update.trusted !== undefined) next.trusted = update.trusted;
    if (update.blocked !== undefined) next.blocked = update.blocked;
    if (update.enabled !== undefined || update.blocked !== undefined) next.inactive = inactiveAfter(next);
    if (update.profiles) next.profiles = [...update.profiles.profiles];
    for (const i of update.integrations)
      if (i.enabled !== undefined && next.hidpp && i.kind === next.hidpp.kind) next.hidpp = { ...next.hidpp, enabled: i.enabled };
    this.devices.set(next.id, next);
    void this.#nameReferences();
  }

  #putSettings(device: number, list: WireSetting[]) {
    this.#wireSettings.set(device, list);
    this.settings.set(device, convert.settings(list));
  }

  #putWarnings(device: number, list: WireWarning[]) {
    this.#wireWarnings.set(device, list);
    this.warnings.set(device, list.map(convert.warning));
  }

  #event(event: Event) {
    const kind = event.kind;
    switch (kind.case) {
      case "adapter": {
        const wasReady = this.status?.ready;
        this.#setStatus(kind.value);
        // Devices listed before the adapter was ready may be incomplete. Before open() returns,
        // run() lists them anyway.
        if (wasReady === false && this.status.ready && this.#connection) this.#sync();
        break;
      }
      case "device":
        this.#putDevice(convert.device(kind.value), true);
        break;
      case "deviceRemoved":
        this.#remove(kind.value.id);
        break;
      case "settingsChanged": {
        const { device, changed, removed } = kind.value;
        if (!this.devices.has(device)) break;
        const touched = (s: WireSetting) => changed.some((c) => compareSettingRefs(settingRef(c), settingRef(s)) === 0) || removed.some((r) => compareSettingRefs(r, settingRef(s)) === 0);
        const list = (this.#wireSettings.get(device) ?? []).filter((s) => !touched(s));
        this.#putSettings(device, [...list, ...changed].sort((a, b) => compareSettingRefs(settingRef(a), settingRef(b))));
        break;
      }
      case "warningsChanged": {
        const { device, added, removed } = kind.value;
        if (!this.devices.has(device)) break;
        // Warnings are a set: adding one already listed, or removing one never listed, changes nothing.
        const list = (this.#wireWarnings.get(device) ?? []).filter((w) => !removed.some((r) => compareWarnings(r, w) === 0));
        for (const w of added) if (!list.some((x) => compareWarnings(x, w) === 0)) list.push(w);
        this.#putWarnings(device, list.sort(compareWarnings));
        break;
      }
      case "profile":
        this.#profileChanged(convert.profile(kind.value));
        break;
      case "profileRemoved":
        this.#profileRemoved(kind.value.id);
        break;
      default:
        this.#hooks.event(event);
        return;
    }
    this.#hooks.changed();
  }

  #named(p: Profile) {
    this.#unnamed.delete(p.id);
    this.profileNames.set(p.id, p);
  }

  /** Updates the shown pages for a created, copied or changed profile. */
  #profileChanged(p: Profile) {
    this.#named(p);
    for (const view of ["list", "picker"] as const) {
      const page = this.#pages[view];
      if (!page) continue;
      const i = page.profiles.findIndex((x) => x.id === p.id);
      if (i !== -1) page.profiles = page.profiles.map((x, n) => (n === i ? p : x));
      // A new profile in the shown page's range belongs on it, in ID order.
      else if (inPage(p.id, page.cursors.at(-1) ?? 0, page.next)) page.profiles = [...page.profiles, p].sort((a, b) => a.id - b.id);
    }
  }

  #profileRemoved(id: number) {
    this.#unnamed.delete(id);
    this.profileNames.delete(id);
    for (const view of ["list", "picker"] as const) {
      const page = this.#pages[view];
      if (!page || !page.profiles.some((p) => p.id === id)) continue;
      page.profiles = page.profiles.filter((p) => p.id !== id);
      // A page left empty is read again, which moves on to what follows or back to what precedes it.
      if (!page.profiles.length && !page.unreadable.length) this.#refreshPage(view);
    }
  }

  /** Reads a shown page again after a profile event, unless a read is already under way: an
   * explicit Next or Previous in flight keeps its result. */
  #refreshPage(view: PageView) {
    if (!this.#pages[view]?.loading) void this.readPage("again", view);
  }

  #setStatus(status: Parameters<typeof convert.status>[0]) {
    const next = convert.status(status);
    // Another adapter can't answer on this session; keep the identity it opened with.
    if (!this.status || next.id === this.status.id) this.status = next;
    void this.#nameReferences();
  }

  #putDevice(device: DeviceRecord, event: boolean) {
    const known = this.devices.has(device.id);
    this.devices.set(device.id, device);
    // A newly paired device's lists are read once; events keep them current.
    if (!known && event && this.listed) void this.#readLists(device.id);
    void this.#nameReferences();
  }

  #remove(id: number) {
    this.devices.delete(id);
    this.settings.delete(id);
    this.#wireSettings.delete(id);
    this.settingsErrors.delete(id);
    this.warnings.delete(id);
    this.#wireWarnings.delete(id);
    this.warningsErrors.delete(id);
  }

  /** Ends the session; the adapter stops any scan and unsaved pairing. */
  async close() {
    await this.#connection.close();
  }
}
