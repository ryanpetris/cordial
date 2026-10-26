// One adapter's session: a Connection and the adapter's state, kept current
// from responses and events in the order the adapter sent them. Each device
// record, profile and settings object arrives whole, so the latest one wins.
// Each listing is complete and replaces what was known of its kind, and
// settings events replace or remove only the objects they name. A successful
// change command means the adapter saved exactly what was sent, so its values
// are applied here without reading them back. Opening lists the devices and,
// when the adapter has profiles, the profiles, then each device's settings.
// The Profiles list and the profile picker show the listed profiles a page at
// a time; profiles in a device's layers or selected by a configuration
// interface that the listing hasn't named are named with GetProfile. Nothing is listed until the
// adapter is ready, and an adapter whose saved data this app can't use gets no request after its
// opening status.
import { Connection, ConnectionClosedError, CordialError, UnexpectedResponseError, settingsRef, type ByteStream } from "@cordial/client";
import { OutputMode, Platform, type Event, type Request, type Response, type SetAdapter, type SetDevice } from "@cordial/protocol";
import type { AdapterStatus, DeviceRecord, Profile, ProfilePage, Setting, SettingsChange, WireError } from "../shared/state.ts";
import { sameField } from "../shared/settings.ts";
import { asSentence, errorText } from "../shared/text.ts";
import * as convert from "./convert.ts";
import { storageGate, type StorageGate } from "./migration.ts";

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
  error instanceof CordialError ? errorText(convert.wireError(error), read ? "read" : "save")
    : error instanceof ConnectionClosedError
      ? read ? "The adapter disconnected before it answered." : "The adapter couldn't confirm whether the change was saved."
      : error instanceof UnexpectedResponseError ? "The adapter returned an unexpected result."
      : asSentence((error as Error).message);

/** A setting as the adapter lists it after SetSettings saved or forgot `change`'s value. */
const changedSetting = (s: Setting, change: SettingsChange): Setting =>
  change.type === "forget" ? { ...s, saved: null, state: null, error: null } : { ...s, saved: change.value, state: "pending", error: null };

/** Which list of profiles a page belongs to: the adapter's Profiles list, or the profile picker,
 * which pages on its own. */
export type PageView = "list" | "picker";

/** Profiles shown on one page of the Profiles list or the profile picker. */
export const PROFILES_PER_PAGE = 10;

/** The adapter's profiles from the latest listing, kept current by profile events. */
interface ProfileListing {
  /** In ID order. */
  profiles: Profile[];
  /** Profiles whose record the adapter couldn't read, with why, in ID order. */
  unreadable: { id: number; error: WireError }[];
  loading: boolean;
  error: string | null;
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
  /** Whether this app can use the adapter's saved data, from its latest status. */
  gate: StorageGate = { kind: "unknown" };
  readonly devices = new Map<number, DeviceRecord>();
  /** What has been listed and reported of each device's settings, in the adapter's order. */
  readonly settings = new Map<number, Setting[]>();
  /** Devices whose settings list has succeeded on the current ready transport. */
  readonly settingsAvailable = new Set<number>();
  /** Whether the profile listing succeeded on the current ready transport. */
  profilesAvailable = false;
  readonly #currentProfiles = new Set<number>();
  readonly settingsErrors = new Map<number, string>();
  /** Why the latest device listing couldn't read a device's record, until the device's next
   * record arrives. */
  readonly readErrors = new Map<number, WireError>();
  /** The devices have been listed. */
  listed = false;
  /** Why the latest device listing failed, until the next one starts. */
  listError: string | null = null;
  /** Profiles seen this session, by ID. */
  readonly profileNames = new Map<number, Profile>();
  #profiles: ProfileListing | null = null;
  /** The page each view shows, kept within the listing as it shrinks; null before the view
   * first opens. */
  readonly #pages: Record<PageView, number | null> = { list: null, picker: null };
  /** IDs being named with GetProfile. */
  readonly #naming = new Set<number>();
  /** IDs the adapter couldn't name this session; asked again only after the profile changes. */
  readonly #unnamed = new Set<number>();

  readonly #hooks: SessionHooks;
  #connection!: Connection;
  #receiver: { session: AdapterSession; closing: Promise<void> | null } = { session: this, closing: null };
  /** Whether device actions can use the current transport. */
  available = false;
  /** Commands in flight, per device. */
  readonly #pending = new Map<number, string[]>();
  #syncing: Promise<void> | null = null;
  #syncAgain = false;

  private constructor(hooks: SessionHooks) {
    this.#hooks = hooks;
    this.status = null as unknown as AdapterStatus;
  }

  /** Starts a session and reads the adapter's status; rejects if the port isn't a working adapter
   * on the current protocol. */
  static async open(stream: ByteStream, hooks: SessionHooks, openTimeoutMs?: number): Promise<AdapterSession> {
    const session = new AdapterSession(hooks);
    const receiver = session.#receiver;
    session.#connection = await Connection.open({
      onData: (listener) => stream.onData(listener),
      onClose: (listener) => stream.onClose(listener),
      write: (bytes) => stream.write(bytes),
      close: () => receiver.closing ??= stream.close(),
    }, {
      onEvent: (event) => receiver.session.#event(event),
      onResponse: (request, response) => receiver.session.#response(request, response),
      onClose: (error) => {
        receiver.session.#unavailable();
        receiver.session.#hooks.closed(error);
      },
      log: (message) => receiver.session.#hooks.log(message),
      openTimeoutMs,
    });
    // Callbacks during the handshake have already applied the status and any later adapter event.
    if (!session.status) session.#setStatus(session.#connection.status);
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

  /** Replaces a failed transport while keeping the session's authoritative records and pages. */
  replace(opened: AdapterSession) {
    if (!this.closed || opened.adapterId !== this.adapterId || opened.closed) throw new ConnectionClosedError();
    this.#connection = opened.#connection;
    this.#receiver = opened.#receiver;
    this.#receiver.session = this;
    this.status = opened.status;
    this.gate = opened.gate;
    this.#unavailable();
    this.#syncing = null;
    this.#syncAgain = false;
    this.listError = null;
    this.#naming.clear();
    this.#unnamed.clear();
    this.run();
  }

  #unavailable() {
    this.available = false;
    this.settingsAvailable.clear();
    this.profilesAvailable = false;
    this.#currentProfiles.clear();
  }

  /** Whether a profile has a readable record on the current transport. */
  profileAvailable(id: number): boolean {
    return this.profilesAvailable && this.#currentProfiles.has(id);
  }

  /** Reads the devices and keeps them current. */
  run() {
    this.#sync();
  }

  /** Sends a command for a device, tracking it as pending while it runs. */
  async perform<T>(device: number, command: string, run: (c: Connection) => Promise<T>): Promise<T> {
    if (!this.available || this.closed) throw new ConnectionClosedError();
    const connection = this.#connection;
    const list = this.#pending.get(device) ?? [];
    list.push(command);
    this.#pending.set(device, list);
    this.#hooks.changed();
    try {
      return await run(connection);
    } finally {
      list.splice(list.indexOf(command), 1);
      if (!list.length) this.#pending.delete(device);
      this.#hooks.changed();
    }
  }

  /** Whether this session is still reading its devices for the first time. */
  get listing(): boolean {
    return !this.listed && this.#syncing !== null;
  }

  pendingFor(id: number): string[] {
    return [...(this.#pending.get(id) ?? [])];
  }

  /** Lists the devices, then the profiles and each device's settings (single flight). */
  #sync() {
    if (this.#syncing) {
      this.#syncAgain = true;
      return;
    }
    const connection = this.#connection;
    if (connection.closed || this.gate.kind !== "current" || !this.status.ready) return;
    this.#syncing = (async () => {
      do {
        this.#syncAgain = false;
        if (this.listError) {
          this.listError = null;
          this.#hooks.changed();
        }
        try {
          // The response replaces the devices as it arrives.
          await connection.listDevices();
          if (connection !== this.#connection || connection.closed) break;
          if (!this.available) break;
          this.listed = true;
          this.#hooks.changed();
          if (this.status.profileSupport) {
            this.#pages.list ??= 0;
            await this.#readProfiles();
            if (connection !== this.#connection || connection.closed) break;
            await this.#nameReferences();
          }
          for (const id of [...this.devices.keys()]) {
            if (connection !== this.#connection || connection.closed) break;
            await this.#readSettings(id);
          }
        } catch (error) {
          if (connection !== this.#connection || connection.closed) break;
          this.listError = failure(error, true);
          this.#hooks.log(`listing devices failed: ${this.listError}`);
        }
      } while (this.#syncAgain && connection === this.#connection && !connection.closed);
      if (connection !== this.#connection) return;
      this.#syncing = null;
      this.#hooks.changed();
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

  /** The last page of the listing. */
  get #lastPage(): number {
    const count = (this.#profiles?.profiles.length ?? 0) + (this.#profiles?.unreadable.length ?? 0);
    return Math.max(0, Math.ceil(count / PROFILES_PER_PAGE) - 1);
  }

  /** Moves each view back to the last page when the listing no longer reaches its page. */
  #clampPages() {
    for (const view of ["list", "picker"] as const) {
      const index = this.#pages[view];
      if (index !== null) this.#pages[view] = Math.min(index, this.#lastPage);
    }
  }

  #pageOf(view: PageView): ProfilePage | null {
    const listing = this.#profiles;
    const index = this.#pages[view];
    if (!listing || index === null || !this.status.profileSupport) return null;
    const all = [...listing.profiles.map((p) => ({ id: p.id, p })), ...listing.unreadable].sort((a, b) => a.id - b.id);
    const shown = all.slice(index * PROFILES_PER_PAGE, (index + 1) * PROFILES_PER_PAGE);
    return {
      profiles: shown.flatMap((e) => ("p" in e ? [e.p] : [])),
      unreadable: shown.flatMap((e) => ("error" in e ? [e] : [])),
      previous: index > 0,
      next: all.length > (index + 1) * PROFILES_PER_PAGE,
      loading: listing.loading,
      error: listing.error,
    };
  }

  /** Shows the first, next or previous page of profiles, or the shown one again. The first page
   * and the shown one again list the profiles again; moving between pages uses what was listed. */
  async readPage(page: "first" | "next" | "previous" | "again", view: PageView = "list") {
    if (!this.available || !this.status.profileSupport || this.closed) return;
    const index = this.#pages[view] ?? 0;
    this.#pages[view] = page === "first" ? 0 : page === "next" ? Math.min(index + 1, this.#lastPage) : page === "previous" ? Math.max(0, index - 1) : index;
    if (page === "first" || page === "again" || !this.#profiles) await this.#readProfiles();
    else this.#hooks.changed();
  }

  /** Lists the profiles. The response replaces the listing as it arrives; a failure leaves what
   * was listed before. */
  async #readProfiles() {
    const connection = this.#connection;
    const listing = (this.#profiles ??= { profiles: [], unreadable: [], loading: false, error: null });
    this.profilesAvailable = false;
    listing.loading = true;
    this.#hooks.changed();
    try {
      await connection.listProfiles();
    } catch (error) {
      if (connection !== this.#connection || connection.closed) return;
      listing.error = failure(error, true);
      this.#hooks.log(`profiles unavailable: ${listing.error}`);
    } finally {
      if (connection !== this.#connection) return;
      listing.loading = false;
      this.#hooks.changed();
    }
  }

  /** Lists the profiles again, as after a failed read. */
  reloadProfiles() {
    void this.readPage("again");
  }

  /** Names the profiles in devices' layers and interfaces that nothing has named yet. */
  async #nameReferences() {
    if (!this.available || !this.status.profileSupport || !this.listed || this.closed) return;
    const connection = this.#connection;
    const ids = this.status.interfaces.map((i) => i.profile);
    for (const d of this.devices.values()) ids.push(...(d.profiles ?? []));
    for (const id of new Set(ids)) {
      if (connection !== this.#connection || connection.closed) return;
      if (!id || this.#currentProfiles.has(id) || this.#naming.has(id) || this.#unnamed.has(id)) continue;
      this.#naming.add(id);
      try {
        await connection.getProfile(id);
      } catch (error) {
        if (connection !== this.#connection || connection.closed) return;
        this.#unnamed.add(id);
        this.#hooks.log(`profile ${id} unavailable: ${failure(error, true)}`);
      } finally {
        if (connection === this.#connection) this.#naming.delete(id);
      }
    }
  }

  async #readSettings(id: number) {
    if (!this.available || !this.devices.has(id) || this.closed) return;
    this.settingsAvailable.delete(id);
    this.#hooks.changed();
    const connection = this.#connection;
    await connection.listSettings(id).then(
      () => { if (connection === this.#connection && !connection.closed) this.settingsErrors.delete(id); },
      (error: unknown) => {
        if (connection !== this.#connection || connection.closed || !this.devices.has(id)) return;
        const reason = failure(error, true);
        this.#hooks.log(`settings of ${id} unavailable: ${reason}`);
        this.settingsErrors.set(id, reason);
      },
    );
    this.#hooks.changed();
  }

  /** Reads a device's settings list again, as after a failed read, or lists the devices again
   * when its record couldn't be read. */
  reload(id: number) {
    if (this.readErrors.has(id)) this.#sync();
    else void this.#readSettings(id);
  }

  /** Lists the devices again, as after records couldn't be read. */
  relist() {
    this.#sync();
  }

  /** Devices whose record the latest listing couldn't read and that this session has no record
   * of, in ID order. */
  get unreadableDevices(): { id: number; error: WireError }[] {
    return [...this.readErrors].filter(([id]) => !this.devices.has(id)).sort(([a], [b]) => a - b).map(([id, error]) => ({ id, error }));
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
        if (!this.status.ready) break;
        // The listing replaces the devices. A device whose record couldn't be read keeps what was
        // known of it, as last known: a failed read is no proof that it is gone.
        const { entries } = result.value;
        const devices = entries.flatMap((e) => (e.entry.case === "device" ? [e.entry.value] : []));
        const unreadable = entries.flatMap((e) => (e.entry.case === "error" ? [e.entry.value] : []));
        const listed = new Set([...devices.map((d) => d.id), ...unreadable.map((u) => u.id)]);
        for (const id of [...this.devices.keys()]) if (!listed.has(id)) this.#remove(id);
        for (const d of devices) this.devices.set(d.id, convert.device(d));
        this.readErrors.clear();
        for (const u of unreadable) {
          const error = convert.recordError(u);
          this.readErrors.set(u.id, error);
          this.#hooks.log(`device ${u.id} unreadable: ${errorText(error, "read")}`);
        }
        this.available = true;
        break;
      }
      case "device":
        this.#putDevice(convert.device(result.value), false);
        break;
      case "settings": {
        if (!this.available) break;
        const { device, objects } = result.value;
        if (!this.devices.has(device)) break;
        this.settings.set(device, convert.settings(objects).sort(convert.compareSettings));
        this.settingsAvailable.add(device);
        break;
      }
      case "profile":
        this.#named(convert.profile(result.value));
        break;
      case "profiles": {
        if (!this.available) break;
        const { entries } = result.value;
        const profiles = entries.flatMap((e) => (e.entry.case === "profile" ? [convert.profile(e.entry.value)] : []));
        this.#currentProfiles.clear();
        for (const p of profiles) this.#named(p);
        this.profilesAvailable = true;
        const listing = (this.#profiles ??= { profiles: [], unreadable: [], loading: false, error: null });
        listing.profiles = profiles.sort((a, b) => a.id - b.id);
        listing.unreadable = entries
          .flatMap((e) => (e.entry.case === "error" ? [{ id: e.entry.value.id, error: convert.recordError(e.entry.value) }] : []))
          .sort((a, b) => a.id - b.id);
        listing.error = null;
        this.#clampPages();
        break;
      }
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
        const list = this.settings.get(device);
        if (!list || !this.devices.has(device)) break;
        // In order, so the last change to each field wins.
        const applied = changes.flatMap(convert.updateChanges);
        this.settings.set(device, list.map((s) => applied.reduce((x, c) => (sameField(c.setting, x) ? changedSetting(x, c) : x), s)));
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
    // The adapter keeps no entry for Disabled. Output state follows in the device event.
    for (const p of update.outputPreferences) {
      const role = convert.role(p.role);
      if (!role) continue;
      const rest = next.outputPreferences.filter((x) => x.role !== role);
      next.outputPreferences = p.mode === OutputMode.DISABLED ? rest : [...rest, { role, mode: convert.outputMode(p.mode) }];
    }
    this.devices.set(next.id, next);
    void this.#nameReferences();
  }

  #event(event: Event) {
    const kind = event.kind;
    // A replacement transport's listing confirms which device IDs are current.
    if (!this.available && kind.case !== "adapter") return;
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
        // A changed object replaces every field of the object with its identity, saved ones included.
        const refs = [...changed.flatMap((o) => settingsRef(o) ?? []), ...removed];
        const list = (this.settings.get(device) ?? []).filter((s) => !refs.some((r) => convert.inObject(s, r)));
        this.settings.set(device, [...list, ...convert.settings(changed)].sort(convert.compareSettings));
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
    this.#currentProfiles.add(p.id);
    this.#unnamed.delete(p.id);
    this.profileNames.set(p.id, p);
  }

  /** Puts a created, copied or changed profile in the listing. */
  #profileChanged(p: Profile) {
    this.#named(p);
    const listing = this.#profiles;
    if (!listing) return;
    listing.profiles = [...listing.profiles.filter((x) => x.id !== p.id), p].sort((a, b) => a.id - b.id);
    listing.unreadable = listing.unreadable.filter((u) => u.id !== p.id);
  }

  #profileRemoved(id: number) {
    this.#currentProfiles.delete(id);
    this.#unnamed.delete(id);
    this.profileNames.delete(id);
    const listing = this.#profiles;
    if (!listing) return;
    listing.profiles = listing.profiles.filter((p) => p.id !== id);
    listing.unreadable = listing.unreadable.filter((u) => u.id !== id);
    this.#clampPages();
  }

  #setStatus(status: Parameters<typeof convert.status>[0]) {
    const next = convert.status(status);
    // Another adapter can't answer on this session; keep the identity it opened with.
    if (!this.status || next.id === this.status.id) {
      this.status = next;
      this.gate = storageGate(status);
    }
    if (!this.status.ready) this.#unavailable();
    void this.#nameReferences();
  }

  #putDevice(device: DeviceRecord, event: boolean) {
    const known = this.devices.has(device.id);
    this.devices.set(device.id, device);
    this.readErrors.delete(device.id);
    // A newly paired device's settings are read once; events keep them current.
    if (!known && event && this.listed) void this.#readSettings(device.id);
    void this.#nameReferences();
  }

  #remove(id: number) {
    this.devices.delete(id);
    this.settings.delete(id);
    this.settingsAvailable.delete(id);
    this.settingsErrors.delete(id);
    this.readErrors.delete(id);
  }

  /** Ends the session; the adapter stops any scan and unsaved pairing. */
  async close() {
    this.#unavailable();
    await this.#connection.close();
    await this.#receiver.closing?.catch(() => {});
  }
}
