import { ErrorCode } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { isAction } from "../src/core/actions.ts";
import { FakeAdapter, device, profile, scale } from "../src/fake/adapter.ts";
import { canEnable, copyName, interfaceProblem, profileInUse, reconnectsUsb, stagedInterfaces } from "../src/shared/profiles.ts";
import type { AdapterStatus, AppState, DeviceEntry } from "../src/shared/state.ts";
import { profileAlertText } from "../src/shared/text.ts";
import { controller, record, until } from "./helpers.ts";

const ID = "0123456789ABCDEF";

async function start(options: ConstructorParameters<typeof FakeAdapter>[0] = {}) {
  const fake = new FakeAdapter({ adapterId: ID, devices: [device(1, { state: "connected" }), device(2, { kinds: ["mouse"], state: "connected" })], ...options });
  const harness = controller({ "/a": fake });
  await harness.c.manager.rescan();
  await until(() => {
    const a = harness.state()?.adapters[0];
    return !!a && a.readiness === "ready" && harness.state()!.devices.length === fake.devices.length
      && (!a.status?.profileSupport || (!!a.profilePage && !a.profilePage.loading));
  });
  return { fake, ...harness, state: harness.state as () => AppState };
}

const adapter = (state: () => AppState) => state().adapters[0]!;
const page = (state: () => AppState) => adapter(state).profilePage!;
const ids = (state: () => AppState) => page(state).profiles.map((p) => p.id);
const commands = (fake: FakeAdapter) => fake.received.map((r) => r.command.case);
const KEY = `${ID}/1`;

describe("Profiles", () => {
  it("lists nothing on a board without profile support and refuses profile actions before sending", async () => {
    const { fake, c, state } = await start({ board: "pico_w" });
    expect(adapter(state).status).toMatchObject({ profileSupport: null, interfaces: [] });
    expect(adapter(state).profilePage).toBeNull();
    expect(state().devices[0]!.device.profiles).toBeNull();
    const sent = fake.received.length;
    expect((await c.act({ type: "profile.create", adapterId: ID, name: "Work" })).ok).toBe(false);
    expect((await c.act({ type: "device.update", key: KEY, profiles: [] })).ok).toBe(false);
    expect((await c.act({ type: "adapter.settings", adapterId: ID, interfaces: { 1: { enabled: false } } })).ok).toBe(false);
    expect(fake.received.length).toBe(sent);
    expect(commands(fake)).not.toContain("listProfiles");
    await c.stop();
  });

  it("creates, copies and deletes profiles at once, following their events", async () => {
    const { fake, c, state } = await start();
    expect(page(state)).toEqual({ profiles: [], unreadable: [], previous: false, next: false, loading: false, error: null });
    expect(await c.act({ type: "profile.create", adapterId: ID, name: "Work" })).toEqual({ ok: true });
    await until(() => ids(state).length === 1);
    expect(fake.profiles).toMatchObject([{ id: 1, name: "Work", rules: [] }]);
    expect(page(state).profiles).toEqual([{ id: 1, name: "Work", roles: [] }]);
    expect(await c.act({ type: "profile.copy", adapterId: ID, profile: 1, name: copyName("Work") })).toEqual({ ok: true });
    await until(() => ids(state).length === 2);
    expect(page(state).profiles[1]).toEqual({ id: 2, name: "Work Copy", roles: [] });
    expect(await c.act({ type: "profile.delete", adapterId: ID, profile: 2 })).toEqual({ ok: true });
    await until(() => ids(state).length === 1);
    expect(adapter(state).profileNames[2]).toBeUndefined();
    // Names are checked before sending.
    const sent = fake.received.length;
    expect(await c.act({ type: "profile.create", adapterId: ID, name: " " })).toEqual({ ok: false, message: "Enter a profile name of up to 64 bytes." });
    expect(fake.received.length).toBe(sent);
    // The desktop never reads or changes rules.
    expect(commands(fake).filter((x) => x === "listProfileRules" || x === "setProfileRules")).toEqual([]);
    await c.stop();
  });

  it("shows one page at a time and moves between pages", async () => {
    const profiles = [1, 2, 3, 4, 5].map((id) => profile(id, `P${id}`));
    const { fake, c, state } = await start({ profiles, pageSize: 2 });
    expect(ids(state)).toEqual([1, 2]);
    expect(page(state)).toMatchObject({ previous: false, next: true });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual([3, 4]);
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual([5]);
    expect(page(state)).toMatchObject({ previous: true, next: false });
    await c.request({ type: "profiles.page", adapterId: ID, page: "previous" });
    expect(ids(state)).toEqual([3, 4]);
    // A failed read leaves the shown page and its cursors as they were.
    fake.failures.listProfiles = [ErrorCode.BUSY];
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(page(state)).toMatchObject({ previous: true, next: true, error: expect.stringContaining("busy") });
    expect(ids(state)).toEqual([3, 4]);
    await c.request({ type: "profiles.page", adapterId: ID, page: "previous" });
    expect(ids(state)).toEqual([1, 2]);
    expect(page(state)).toMatchObject({ previous: false, error: null });
    const afters = fake.received.flatMap((r) => (r.command.case === "listProfiles" ? [r.command.value.after] : []));
    expect(afters).toEqual([0, 2, 4, 2, 4, 0]);
    await c.stop();
  });

  it("keeps the cursors when moving back from an emptied next page fails", async () => {
    const profiles = [1, 2, 3, 4, 5, 6].map((id) => profile(id, `P${id}`));
    const { fake, c, state } = await start({ profiles, pageSize: 2 });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual([3, 4]);
    // The following page empties without the adapter saying so, and reading the shown one fails.
    fake.profiles = fake.profiles.filter((p) => p.id < 5);
    fake.failures.listProfiles = [undefined as unknown as ErrorCode, ErrorCode.BUSY];
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(page(state)).toMatchObject({ previous: true, error: expect.stringContaining("busy") });
    expect(ids(state)).toEqual([3, 4]);
    await c.request({ type: "profiles.page", adapterId: ID, page: "previous" });
    expect(ids(state)).toEqual([1, 2]);
    await c.stop();
  });

  it("pages on after the last profile listing entry of a known kind", async () => {
    const fake = new FakeAdapter({ adapterId: ID, devices: [], profiles: [1, 2, 3].map((id) => profile(id, `P${id}`)), pageSize: 2 });
    fake.newerProfiles.add(2);
    const harness = controller({ "/a": fake });
    const { c } = harness;
    const state = harness.state as () => AppState;
    await c.manager.rescan();
    await until(() => !!state()?.adapters[0]?.profilePage && !page(state).loading);
    expect(ids(state)).toEqual([1]);
    expect(page(state)).toMatchObject({ next: true });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual([3]);
    const afters = fake.received.flatMap((r) => (r.command.case === "listProfiles" ? [r.command.value.after] : []));
    expect(afters).toEqual([0, 1]);
    await c.stop();
  });

  it("pages the profile picker separately from the Profiles list", async () => {
    const profiles = [1, 2, 3, 4, 5].map((id) => profile(id, `P${id}`));
    const { c, state } = await start({ profiles, pageSize: 2 });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual([3, 4]);
    expect(adapter(state).pickerPage).toBeNull();
    const picked = () => adapter(state).pickerPage?.profiles.map((p) => p.id);
    await c.request({ type: "profiles.page", adapterId: ID, page: "first", picker: true });
    expect(picked()).toEqual([1, 2]);
    await c.request({ type: "profiles.page", adapterId: ID, page: "next", picker: true });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next", picker: true });
    expect(picked()).toEqual([5]);
    expect(adapter(state).pickerPage).toMatchObject({ previous: true, next: false });
    expect(ids(state)).toEqual([3, 4]);
    expect(page(state)).toMatchObject({ previous: true, next: true });
    // Profile events update both pages.
    await c.act({ type: "profile.delete", adapterId: ID, profile: 5 });
    await until(() => picked()?.join() === "3,4" && !adapter(state).pickerPage!.loading);
    expect(ids(state)).toEqual([3, 4]);
    await c.stop();
  });

  it("moves back a page when deleting leaves the shown one empty, and places new profiles", async () => {
    const { c, state } = await start({ profiles: [profile(1, "A"), profile(2, "B"), profile(3, "C")], pageSize: 2 });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual([3]);
    expect(await c.act({ type: "profile.create", adapterId: ID, name: "D" })).toEqual({ ok: true });
    await until(() => ids(state).length === 2);
    expect(ids(state)).toEqual([3, 4]);
    await c.act({ type: "profile.delete", adapterId: ID, profile: 3 });
    await c.act({ type: "profile.delete", adapterId: ID, profile: 4 });
    await until(() => ids(state).join() === "1,2" && !page(state).loading);
    expect(page(state)).toMatchObject({ previous: false, next: false });
    await c.stop();
  });

  it("lists a profile the adapter couldn't read on its page", async () => {
    const fake = new FakeAdapter({ adapterId: ID, devices: [], profiles: [profile(1, "A"), profile(2, "B")] });
    fake.unreadableProfiles.add(2);
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => !!state()?.adapters[0]?.profilePage && !state()!.adapters[0]!.profilePage!.loading);
    expect(state()!.adapters[0]!.profilePage).toMatchObject({ profiles: [{ id: 1 }], unreadable: [2] });
    await c.stop();
  });

  it("shows each profile's roles and follows a change of roles", async () => {
    const { fake, c, state } = await start({ profiles: [profile(1, "Typing", ["keyboard", "consumer_control"])] });
    expect(page(state).profiles[0]!.roles).toEqual(["keyboard", "consumer_control"]);
    // Another client adds a scale rule on the wheel.
    const session = c.manager.connected.get(ID)!.session;
    await session.connection.setProfileRules({ profile: 1, changes: [{ change: { case: "rule", value: scale([0x01, 0x38], 2) } }] });
    await until(() => page(state).profiles[0]!.roles.length === 3);
    expect(page(state).profiles[0]!.roles).toEqual(["keyboard", "mouse", "consumer_control"]);
    expect(fake.profiles[0]!.rules).toHaveLength(3);
    await c.stop();
  });

  it("names the profiles in layers and interfaces that no page has shown", async () => {
    const profiles = [1, 2, 3].map((id) => profile(id, `P${id}`, ["keyboard"]));
    const fake = new FakeAdapter({
      adapterId: ID,
      devices: [device(1, { profiles: [3, 9] })],
      profiles,
      pageSize: 1,
      interfaces: { vial: { enabled: false, profile: 2 } },
    });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => !!state()?.adapters[0]?.profileNames[3] && !!state()!.adapters[0]!.profileNames[2]);
    expect(state()!.adapters[0]!.profileNames[3]).toEqual({ id: 3, name: "P3", roles: ["keyboard"] });
    // A reference to a profile that no longer exists is asked about once.
    fake.changeDevice(1, { name: "Renamed" });
    await until(() => state()!.devices[0]!.name === "Renamed");
    const reads = fake.received.flatMap((r) => (r.command.case === "getProfile" ? [r.command.value.profile] : []));
    expect(reads.sort()).toEqual([2, 3, 9]);
    expect(state()!.adapters[0]!.profileNames[9]).toBeUndefined();
    await c.stop();
  });

  it("saves a device's switches and layers in one request", async () => {
    const { fake, c, state } = await start({ profiles: [profile(1, "A"), profile(2, "B")], maxLayers: 2 });
    const trusted = !state().devices[0]!.device.trusted;
    const sent = fake.received.length;
    expect(await c.act({ type: "device.update", key: KEY, trusted, profiles: [2, 1] })).toEqual({ ok: true });
    const requests = fake.received.slice(sent).filter((r) => r.command.case === "setDevice");
    expect(requests).toHaveLength(1);
    expect(requests[0]!.command.value).toMatchObject({ device: 1, trusted, profiles: { profiles: [2, 1] } });
    await until(() => state().devices[0]!.device.profiles!.join() === "2,1");
    // Too many layers, or no profile, is refused before sending.
    const before = fake.received.length;
    expect((await c.act({ type: "device.update", key: KEY, profiles: [1, 2, 1] })).ok).toBe(false);
    expect((await c.act({ type: "device.update", key: KEY, profiles: [0] })).ok).toBe(false);
    expect(fake.received.length).toBe(before);
    // An empty list passes everything through.
    expect(await c.act({ type: "device.update", key: KEY, profiles: [] })).toEqual({ ok: true });
    await until(() => state().devices[0]!.device.profiles!.length === 0);
    await c.stop();
  });

  it("refuses to delete a profile in use", async () => {
    const { fake, c, state } = await start({ profiles: [profile(1, "A")], interfaces: { via: { enabled: false, profile: 1 } } });
    const del = () => c.act({ type: "profile.delete", adapterId: ID, profile: 1 });
    // A disabled interface keeps its profile, which still counts as a use.
    expect(await del()).toEqual({ ok: false, message: "VIA is using this profile. Pick a different profile for VIA first." });
    expect(await c.act({ type: "adapter.settings", adapterId: ID, interfaces: { 1: { profile: 0 } } })).toEqual({ ok: true });
    await until(() => adapter(state).status!.interfaces[0]!.profile === 0);
    await c.act({ type: "device.update", key: KEY, profiles: [1] });
    await until(() => state().devices[0]!.device.profiles!.length === 1);
    expect(await del()).toEqual({ ok: false, message: "Keyboard is using this profile. Remove it from Keyboard's profiles first." });
    expect(commands(fake)).not.toContain("deleteProfile");
    await c.act({ type: "device.update", key: KEY, profiles: [] });
    await until(() => state().devices[0]!.device.profiles!.length === 0);
    // The adapter's own refusal is shown as it is.
    fake.failures.deleteProfile = [ErrorCode.IN_USE];
    expect(await del()).toEqual({ ok: false, message: "This profile is still in use. Remove it from every device and interface before deleting it" });
    expect(await del()).toEqual({ ok: true });
    await c.stop();
  });

  it("changes configuration interfaces together, refusing what the adapter would, and reconnects USB", async () => {
    const { fake, c, ports, state } = await start({ profiles: [profile(1, "A")] });
    const sent = fake.received.length;
    expect(await c.act({ type: "adapter.settings", adapterId: ID, interfaces: { 1: { enabled: true } } })).toEqual({
      ok: false,
      message: "Choose a profile for VIA before turning it on.",
    });
    expect(await c.act({ type: "adapter.settings", adapterId: ID, interfaces: { 1: { enabled: true, profile: 1 }, 2: { enabled: true, profile: 1 } } })).toEqual({
      ok: false,
      message: "VIA and Vial can't both be on. Turn one off first.",
    });
    expect(await c.act({ type: "adapter.settings", adapterId: ID, interfaces: { 3: { enabled: false } } })).toMatchObject({ ok: false });
    expect(fake.received.length).toBe(sent);

    expect(await c.act({ type: "adapter.settings", adapterId: ID, platform: "mac", interfaces: { 2: { enabled: true, profile: 1 } } })).toEqual({ ok: true });
    const request = fake.received.filter((r) => r.command.case === "setAdapter").at(-1)!.command.value;
    expect(request).toMatchObject({ platform: 2, configurationInterfaces: [{ interface: 2, enabled: true, profile: 1 }] });
    // The adapter reconnects USB after answering; its page waits for it.
    await until(() => adapter(state).connection === "connecting");
    expect(fake.usbReconnects).toBe(1);
    expect(fake.serial).toBe(`${ID}-vial:f64c2b3c`);
    await until(() => fake.present);
    await c.manager.rescan();
    await until(() => adapter(state).connection === "connected" && adapter(state).readiness === "ready");
    expect(adapter(state).status!.interfaces).toEqual([
      { interface: 1, enabled: false, profile: 0, conflicts: [2] },
      { interface: 2, enabled: true, profile: 1, conflicts: [1] },
    ]);
    // A user-disconnected adapter is recognized by the start of its USB serial number.
    await c.act({ type: "adapter.disconnect", adapterId: ID });
    const opened = ports.opened;
    await c.manager.rescan();
    expect(c.state().adapters[0]!.connection).toBe("disconnected");
    expect(ports.opened).toBe(opened);
    await c.stop();
  });

  it("notifies once when profile memory is nearly full and once when a device's profiles don't load", async () => {
    const { fake, c, deps, state } = await start({ profiles: [profile(1, "Big", [], 900), profile(2, "Small", [], 200)], memoryBudget: 1000 });
    const alerts = () => deps.profileAlert.mock.calls.map(([a]) => a);
    await c.act({ type: "device.update", key: KEY, profiles: [1] });
    await until(() => adapter(state).status!.profileSupport!.memoryUsed === 900);
    await until(() => alerts().length === 1);
    expect(alerts()[0]).toEqual({ kind: "memory", adapterId: ID, name: "Pico 2 W", percent: 90 });
    expect(profileAlertText(alerts()[0]!)).toEqual({ title: "Pico 2 W is almost out of profile memory.", body: null });
    await c.act({ type: "device.update", key: `${ID}/2`, profiles: [2] });
    await until(() => alerts().length === 2);
    expect(alerts()[1]).toEqual({ kind: "device", key: `${ID}/2`, name: "Keyboard", code: "no_capacity" });
    expect(state().devices[1]!.device.profileError).toBe("no_capacity");
    expect(profileAlertText(alerts()[1]!)).toEqual({
      title: "Keyboard connected without its profiles.",
      body: "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles.",
    });
    // Releasing memory loads the waiting device's profiles and clears both conditions.
    await c.act({ type: "device.update", key: KEY, profiles: [] });
    await until(() => state().devices[1]!.device.profileError === null && adapter(state).status!.profileSupport!.memoryUsed === 200);
    expect(alerts()).toHaveLength(2);
    // Each notifies again once the condition returns.
    await c.act({ type: "device.update", key: KEY, profiles: [1] });
    await until(() => state().devices[0]!.device.profileError === "no_capacity");
    await until(() => alerts().length === 3);
    expect(alerts()[2]).toMatchObject({ kind: "device", key: KEY });
    fake.changeAdapter({ memoryBudget: 1100 });
    await until(() => alerts().length === 4);
    expect(alerts()[3]).toMatchObject({ kind: "memory", percent: 100 });
    await c.stop();
  });

  it("validates profile actions from the window", () => {
    expect(isAction({ type: "adapter.settings", adapterId: "a", interfaces: { 1: { enabled: true, profile: 3 } } })).toBe(true);
    expect(isAction({ type: "adapter.settings", adapterId: "a", interfaces: { 1: { profile: -1 } } })).toBe(false);
    expect(isAction({ type: "adapter.settings", adapterId: "a", interfaces: { via: { enabled: true } } })).toBe(false);
    expect(isAction({ type: "adapter.settings", adapterId: "a", interfaces: { 1: { enabled: true, extra: 1 } } })).toBe(false);
    expect(isAction({ type: "adapter.settings", adapterId: "a", platform: "mac", transports: { classic: true, ble: false } })).toBe(true);
    expect(isAction({ type: "device.update", key: "a/1", profiles: [1, 2, 1] })).toBe(true);
    expect(isAction({ type: "device.update", key: "a/1", profiles: ["1"] })).toBe(false);
    expect(isAction({ type: "device.update", key: "a/1", profiles: [1.5] })).toBe(false);
    expect(isAction({ type: "profile.create", adapterId: "a", name: "n" })).toBe(true);
    expect(isAction({ type: "profile.copy", adapterId: "a", profile: 1, name: "n" })).toBe(true);
    expect(isAction({ type: "profile.copy", adapterId: "a", profile: "1", name: "n" })).toBe(false);
    expect(isAction({ type: "profile.delete", adapterId: "a" })).toBe(false);
    expect(isAction({ type: "profiles.page", adapterId: "a", page: "next" })).toBe(true);
    expect(isAction({ type: "profiles.page", adapterId: "a", page: "last" })).toBe(false);
    expect(isAction({ type: "pair.start", adapterId: "a", candidateId: 3 })).toBe(true);
    expect(isAction({ type: "pair.start", adapterId: "a", candidateId: "c_3" })).toBe(false);
  });
});

describe("Configuration interface checks", () => {
  const status = (interfaces: AdapterStatus["interfaces"]): AdapterStatus => ({
    id: ID,
    name: "A",
    platform: "linux",
    ready: true,
    transports: [],
    info: [],
    profileSupport: { memoryBudget: 100, memoryUsed: 0, maxLayers: 4 },
    interfaces,
  });
  const via = { interface: 1, enabled: false, profile: 0, conflicts: [2] };
  const vial = { interface: 2, enabled: false, profile: 0, conflicts: [1] };

  it("reconnects USB when an interface turns on or off, or an enabled one changes profile", () => {
    const off = status([via, vial]);
    expect(reconnectsUsb(off, { interfaces: { 1: { profile: 3 } } })).toBe(false);
    expect(reconnectsUsb(off, { interfaces: { 1: { enabled: true, profile: 3 } } })).toBe(true);
    const on = status([{ ...via, enabled: true, profile: 3 }, vial]);
    expect(reconnectsUsb(on, { interfaces: { 1: { profile: 4 } } })).toBe(true);
    expect(reconnectsUsb(on, { interfaces: { 1: { profile: 3 } } })).toBe(false);
    expect(reconnectsUsb(on, { platform: "mac" })).toBe(false);
  });

  it("offers turning an interface on only with a profile and without a conflicting one on", () => {
    const s = status([{ ...via, enabled: true, profile: 1 }, { ...vial, profile: 1 }]);
    const staged = stagedInterfaces(s, {});
    expect(canEnable(staged, staged[1]!)).toBe(false);
    expect(canEnable(stagedInterfaces(s, { interfaces: { 1: { enabled: false } } }), staged[1]!)).toBe(true);
    expect(canEnable(staged, { ...via, interface: 2, profile: 0, conflicts: [] })).toBe(false);
    // Either side's conflicts list counts.
    expect(interfaceProblem([{ ...via, enabled: true, profile: 1, conflicts: [] }, { ...vial, enabled: true, profile: 1 }])).toBe("VIA and Vial can't both be on. Turn one off first.");
    expect(interfaceProblem(stagedInterfaces(s, { interfaces: { 1: { profile: 0 } } }))).toBe("Choose a profile for VIA before turning it on.");
    expect(interfaceProblem(staged)).toBeNull();
  });

  it("names what uses a profile", () => {
    const entry = (profiles: number[] | null) => ({ name: "Keys", device: record(1, { profiles }) }) as DeviceEntry;
    expect(profileInUse(status([via, { ...vial, profile: 5 }]), [], 5)).toBe("Vial is using this profile. Pick a different profile for Vial first.");
    expect(profileInUse(status([via]), [entry([1, 5])], 5)).toBe("Keys is using this profile. Remove it from Keys's profiles first.");
    expect(profileInUse(status([via]), [entry(null)], 5)).toBeNull();
    expect(copyName("Work")).toBe("Work Copy");
    expect(new TextEncoder().encode(copyName("x".repeat(64))).length).toBe(64);
  });
});
