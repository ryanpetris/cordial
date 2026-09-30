// One adapter's device view, rebuilt from snapshots and kept current by
// monitor events in the adapter's shared revision order (docs/protocol/monitoring.md).
// Pure state: the session does all I/O.
import type {
  Device,
  DeviceInfo,
  ErrorCode,
  Event,
  HostPlatform,
  InfoField,
  Setting,
  SettingsState,
} from "../protocol/types.ts";
import { clean } from "../shared/text.ts";

interface Row<T> {
  revision: number;
  value: T;
}

/** A per-device cache filled by complete snapshots and revisioned deltas. */
class Cache<T> {
  rows = new Map<string, Row<T>>();
  /** Revision of the last installed snapshot. */
  revision = 0;
  /** A snapshot of the current epoch is installed and nothing invalidated it. */
  complete = false;
  /** Epoch of the last read started. */
  epoch = -1;
  /** Epoch whose read failed. */
  failedEpoch = -1;
  /** View revision when the read started, and when its failure was recorded. */
  readRevision = -1;
  failedRevision = -1;
  /** Highest revision of a change that invalidated this cache. */
  staleRevision = 0;

  /** Keeps whichever of the stored and offered value is newer. */
  put(key: string, revision: number, value: T) {
    const old = this.rows.get(key);
    if (!old || old.revision <= revision) this.rows.set(key, { revision, value });
  }

  /** Replaces the rows with a complete snapshot, keeping newer deltas. */
  replace(revision: number, entries: [string, T][]) {
    const rows = new Map(entries.map(([key, value]) => [key, { revision, value }]));
    for (const [key, row] of this.rows) if (row.revision > revision) rows.set(key, row);
    this.rows = rows;
    this.revision = Math.max(this.revision, revision);
  }

  /** Needs a read in this epoch. */
  needed(epoch: number) {
    return this.epoch !== epoch || (!this.complete && this.failedEpoch !== epoch);
  }
}

export interface SettingsView {
  settings: Setting[];
  state: SettingsState | null;
  error: ErrorCode | null;
  /** A complete list has been read since the last invalidation. */
  current: boolean;
  /** Why the last read of the list failed, in words. */
  loadError: string | null;
}

type Change =
  | { kind: "adapter"; revision: number; platform: HostPlatform; name: string }
  | { kind: "device"; revision: number; device: Device; paired: boolean }
  | { kind: "unpaired"; revision: number; id: string }
  | { kind: "info"; revision: number; info: DeviceInfo }
  | { kind: "setting"; revision: number; id: string; setting: Setting };

const infoKey = (f: InfoField) => `${f.key}/${f.instance}`;
const HISTORY_LIMIT = 256;

export class AdapterView {
  devices = new Map<string, Device>();
  revision = 0;
  /** A complete snapshot is installed and no events were lost since. */
  valid = false;
  platform: HostPlatform | null = null;
  name: string | null = null;
  #adapterRevision = -1;
  #lostRevision = 0;
  /** Advances whenever cached device information and settings may be stale. */
  #epoch = 0;
  #buffer: Change[] | null = null;
  #infos = new Map<string, Cache<InfoField>>();
  #settings = new Map<string, Cache<Setting>>();
  #settingsMeta = new Map<string, { state: SettingsState | null; error: ErrorCode | null; loadError: string | null }>();
  /** Why the last information read failed, in words, per device. */
  #infoErrors = new Map<string, string>();
  /** The last valid reported name per device, which a later omission keeps. */
  #names = new Map<string, Row<string>>();

  /** Records adapter settings no older than the current one. */
  setAdapter(revision: number, platform: HostPlatform, name: string) {
    if (revision >= this.#adapterRevision) {
      this.platform = platform;
      this.name = name;
      this.#adapterRevision = revision;
    }
  }

  /** Marks the view out of date after lost or missing events. */
  lose(revision = this.revision) {
    this.#lostRevision = Math.max(this.#lostRevision, revision);
    this.valid = false;
    this.#epoch++;
  }

  /** Buffers events until the snapshot being requested is installed. */
  beginSnapshot() {
    this.#buffer = [];
  }

  /** Installs a complete saved-device snapshot, then replays newer events. */
  installSnapshot(devices: Device[], revision: number) {
    const buffered = this.#buffer ?? [];
    this.#buffer = null;
    this.devices = new Map(devices.map((d) => [d.device_id, d]));
    this.revision = revision;
    this.valid = revision >= this.#lostRevision;
    for (const change of buffered) this.#apply(change);
    for (const id of [...this.#infos.keys()]) if (!this.devices.has(id)) this.#infos.delete(id);
    for (const id of [...this.#infoErrors.keys()]) if (!this.devices.has(id)) this.#infoErrors.delete(id);
    for (const id of [...this.#names.keys()]) if (!this.devices.has(id)) this.#names.delete(id);
    for (const id of [...this.#settings.keys()]) if (!this.devices.has(id)) this.#forgetSettings(id);
  }

  /** Abandons a failed snapshot; buffered events are covered by the next one. */
  abortSnapshot() {
    this.#buffer = null;
    this.lose();
  }

  /** Applies a monitor event. Returns false for events that carry no state. */
  event(event: Event): boolean {
    let change: Change;
    switch (event.event) {
      case "events.lost":
        this.lose(event.data.revision);
        return true;
      case "adapter.changed":
        change = { kind: "adapter", revision: event.data.revision, platform: event.data.host_platform, name: event.data.name };
        break;
      case "device.paired":
      case "device.connected":
      case "device.changed":
      case "device.disconnected":
        change = {
          kind: "device",
          revision: event.data.revision,
          device: event.data.device,
          paired: event.event === "device.paired",
        };
        break;
      case "device.unpaired":
        change = { kind: "unpaired", revision: event.data.revision, id: event.data.device_id };
        break;
      case "device.info.changed":
        change = { kind: "info", revision: event.data.revision, info: event.data };
        break;
      case "hidpp.setting.changed":
        change = {
          kind: "setting",
          revision: event.data.revision,
          id: event.data.device_id,
          setting: event.data.setting,
        };
        break;
      default:
        return false;
    }
    if (this.#buffer) {
      this.#buffer.push(change);
      if (this.#buffer.length > HISTORY_LIMIT) this.lose(this.#buffer.shift()!.revision);
    }
    else this.#apply(change);
    return true;
  }

  #apply(change: Change) {
    // Information and settings rows carry their own revisions, so deltas are
    // merged even when a snapshot of the same data is still in flight.
    switch (change.kind) {
      case "adapter":
        this.setAdapter(change.revision, change.platform, change.name);
        break;
      case "info":
        this.#rememberName(change.info.device_id, change.revision, change.info.fields);
        for (const f of change.info.fields) this.#infos.get(change.info.device_id)?.put(infoKey(f), change.revision, f);
        break;
      case "setting":
        this.#settings.get(change.id)?.put(change.setting.key, change.revision, change.setting);
        break;
      case "unpaired":
        this.#dropCaches(change.id, change.revision);
        if ((this.#names.get(change.id)?.revision ?? -1) < change.revision) this.#names.delete(change.id);
        break;
      case "device": {
        if (change.paired) this.#dropCaches(change.device.device_id, change.revision);
        const settings = this.#settings.get(change.device.device_id);
        // A catalog change, disconnect or reset makes the cached list stale.
        if (settings && change.revision > settings.revision) {
          const old = this.devices.get(change.device.device_id);
          if (
            change.device.settings_revision > settings.revision ||
            change.device.state !== old?.state ||
            change.device.normalization_state === "resetting"
          ) {
            settings.complete = false;
            settings.staleRevision = Math.max(settings.staleRevision, change.revision);
          }
          if (change.device.state !== "connected" || change.device.normalization_state === "resetting") {
            for (const [key, row] of settings.rows) {
              if (row.revision >= change.revision) continue;
              const value = { ...row.value, fresh: false };
              if (value.managed && ["applied", "applying", "changed_on_device"].includes(value.state)) value.state = "pending";
              settings.put(key, change.revision, value);
            }
          }
        }
        break;
      }
    }
    if (change.revision <= this.revision) return;
    if (change.revision !== this.revision + 1) this.lose();
    if (change.kind === "device") this.devices.set(change.device.device_id, change.device);
    else if (change.kind === "unpaired") this.devices.delete(change.id);
    this.revision = change.revision;
  }

  #rememberName(id: string, revision: number, fields: InfoField[]) {
    const name = fields.find((f) => f.key === "name" && f.instance === 0 && f.available)?.value;
    // A name that is empty once cleaned for display doesn't replace one.
    if (typeof name !== "string" || !clean(name)) return;
    if ((this.#names.get(id)?.revision ?? -1) <= revision) this.#names.set(id, { revision, value: name });
  }

  #dropCaches(id: string, revision: number) {
    const info = this.#infos.get(id);
    if (info && revision > info.revision) this.#infos.delete(id);
    const settings = this.#settings.get(id);
    if (settings && revision > settings.revision) this.#forgetSettings(id);
  }

  #forgetSettings(id: string) {
    this.#settings.delete(id);
    this.#settingsMeta.delete(id);
  }

  // ---- Device information --------------------------------------------------

  /** Saved devices whose information snapshot is missing or out of date. */
  infoNeeded(): string[] {
    if (!this.valid) return [];
    return [...this.devices.keys()].filter((id) => this.#infos.get(id)?.needed(this.#epoch) ?? true);
  }

  /** Prepares for an information snapshot so deltas that race it are kept. */
  beginInfo(id: string): number {
    let cache = this.#infos.get(id);
    if (!cache) this.#infos.set(id, (cache = new Cache()));
    if (cache.epoch !== this.#epoch) cache.complete = false;
    cache.epoch = this.#epoch;
    return this.#epoch;
  }

  /** Installs a complete snapshot from `device.info` or `device.info.refresh`. */
  installInfo(info: DeviceInfo, epoch = this.#epoch) {
    if (!this.devices.has(info.device_id)) return;
    let cache = this.#infos.get(info.device_id);
    if (!cache) {
      this.#infos.set(info.device_id, (cache = new Cache()));
      cache.epoch = epoch;
    }
    this.#rememberName(info.device_id, info.revision, info.fields);
    cache.replace(
      info.revision,
      info.fields.map((f) => [infoKey(f), f]),
    );
    if (epoch === this.#epoch && cache.epoch === epoch) {
      cache.complete = true;
      this.#infoErrors.delete(info.device_id);
    }
  }

  /** Records a failed read; it is retried in the next epoch. */
  infoFailed(id: string, epoch: number, reason: string) {
    const cache = this.#infos.get(id);
    if (!cache || cache.epoch !== epoch) return;
    cache.failedEpoch = epoch;
    this.#infoErrors.set(id, reason);
  }

  infoError(id: string): string | null {
    return this.#infoErrors.get(id) ?? null;
  }

  info(id: string): InfoField[] | null {
    const cache = this.#infos.get(id);
    return cache && (cache.complete || cache.rows.size) ? [...cache.rows.values()].map((r) => r.value) : null;
  }

  infoCurrent(id: string): boolean {
    const cache = this.#infos.get(id);
    return this.valid && !!cache?.complete && cache.epoch === this.#epoch;
  }

  /** The last valid name the device reported, even if only last known. */
  reportedName(id: string): string | null {
    return this.#names.get(id)?.value ?? null;
  }

  // ---- Settings ------------------------------------------------------------

  /** Watched devices whose cached settings list must be read again. */
  settingsNeeded(watched: Iterable<string>): string[] {
    if (!this.valid) return [];
    return [...watched].filter((id) => {
      if (!this.devices.has(id)) return false;
      const cache = this.#settings.get(id);
      return !cache || cache.needed(this.#epoch) || (!cache.complete && cache.failedRevision !== this.revision);
    });
  }

  /** Allows a cached-list retry without refreshing the peripheral. */
  retrySettings(id: string, force = true) {
    const cache = this.#settings.get(id);
    if (cache) {
      if (force) cache.complete = false;
      cache.failedEpoch = -1;
      cache.failedRevision = -1;
    }
  }

  beginSettings(id: string): number {
    let cache = this.#settings.get(id);
    if (!cache) this.#settings.set(id, (cache = new Cache()));
    cache.epoch = this.#epoch;
    cache.complete = false;
    cache.readRevision = this.revision;
    return this.#epoch;
  }

  /** Installs a consistent, complete settings list read at `revision`. */
  installSettings(
    id: string,
    revision: number,
    settings: Setting[],
    state: SettingsState,
    error: ErrorCode | null,
    epoch: number,
  ) {
    const cache = this.#settings.get(id);
    if (!cache || !this.devices.has(id)) return;
    cache.replace(
      revision,
      settings.map((s) => [s.key, s]),
    );
    cache.failedEpoch = -1;
    // A change newer than the list that invalidated it needs another read.
    cache.complete = epoch === this.#epoch && cache.epoch === epoch && cache.staleRevision <= revision;
    this.#settingsMeta.set(id, { state, error, loadError: null });
  }

  /** Records a failed read; it is retried in the next epoch. */
  settingsFailed(id: string, epoch: number, reason: string) {
    const cache = this.#settings.get(id);
    if (!cache || cache.epoch !== epoch) return;
    cache.failedEpoch = epoch;
    cache.failedRevision = cache.readRevision;
    const meta = this.#settingsMeta.get(id);
    this.#settingsMeta.set(id, { state: meta?.state ?? null, error: meta?.error ?? null, loadError: reason });
  }

  /** Applies a setting record returned by a command at its revision. */
  putSetting(id: string, revision: number, setting: Setting) {
    if (!this.devices.has(id)) return;
    let cache = this.#settings.get(id);
    if (!cache) this.#settings.set(id, (cache = new Cache()));
    cache.put(setting.key, revision, setting);
  }

  settings(id: string): SettingsView | null {
    const cache = this.#settings.get(id);
    if (!cache) return null;
    const meta = this.#settingsMeta.get(id);
    return {
      settings: [...cache.rows.values()].map((r) => r.value),
      state: meta?.state ?? null,
      error: meta?.error ?? null,
      current: this.valid && cache.complete && cache.epoch === this.#epoch,
      loadError: meta?.loadError ?? null,
    };
  }
}
