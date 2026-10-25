import { create } from "@bufbuild/protobuf";
import { ErrorCode, ProfileRuleSchema, UsageRange_Kind } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { isAction } from "../src/core/actions.ts";
import { FakeAdapter, axis, device, profile, remap, scale } from "../src/fake/adapter.ts";
import { canEnable, copyName, interfaceProblem, profileInUse, stagedInterfaces } from "../src/shared/profiles.ts";
import type { AdapterStatus, AppState, DeviceEntry } from "../src/shared/state.ts";
import { memoryPercent, profileAlertText } from "../src/shared/text.ts";
import { PROFILES_PER_PAGE } from "../src/core/session.ts";
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

  it("shows one page at a time of a single listing and moves between pages", async () => {
    const profiles = Array.from({ length: 25 }, (_, i) => profile(i + 1, `P${i + 1}`));
    const { fake, c, state } = await start({ profiles, batchSize: 4 });
    const range = (from: number, to: number) => Array.from({ length: to - from + 1 }, (_, i) => from + i);
    expect(ids(state)).toEqual(range(1, PROFILES_PER_PAGE));
    expect(page(state)).toMatchObject({ previous: false, next: true });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual(range(11, 20));
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual(range(21, 25));
    expect(page(state)).toMatchObject({ previous: true, next: false });
    await c.request({ type: "profiles.page", adapterId: ID, page: "previous" });
    expect(ids(state)).toEqual(range(11, 20));
    expect(commands(fake).filter((x) => x === "listProfiles")).toHaveLength(1);
    // A listing that fails after some entries leaves the listed profiles and the shown page.
    fake.profiles = fake.profiles.slice(1);
    fake.lateFailures.listProfiles = [ErrorCode.BUSY];
    await c.request({ type: "adapter.reload", adapterId: ID });
    await until(() => !page(state).loading && page(state).error !== null);
    expect(page(state).error).toContain("busy");
    expect(ids(state)).toEqual(range(11, 20));
    await c.request({ type: "profiles.page", adapterId: ID, page: "previous" });
    expect(ids(state)).toEqual(range(1, 10));
    await c.request({ type: "adapter.reload", adapterId: ID });
    await until(() => !page(state).loading && page(state).error === null);
    expect(ids(state)).toEqual(range(2, 11));
    await c.stop();
  });

  it("skips profile listing entries of a kind newer than this app", async () => {
    const fake = new FakeAdapter({ adapterId: ID, devices: [], profiles: [1, 2, 3].map((id) => profile(id, `P${id}`)), batchSize: 2 });
    fake.newerProfiles.add(2);
    const harness = controller({ "/a": fake });
    const { c } = harness;
    const state = harness.state as () => AppState;
    await c.manager.rescan();
    await until(() => !!state()?.adapters[0]?.profilePage && !page(state).loading);
    expect(ids(state)).toEqual([1, 3]);
    expect(page(state)).toMatchObject({ previous: false, next: false });
    await c.stop();
  });

  it("pages the profile picker separately from the Profiles list", async () => {
    const profiles = Array.from({ length: 25 }, (_, i) => profile(i + 1, `P${i + 1}`));
    const { c, state } = await start({ profiles });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)[0]).toBe(11);
    expect(adapter(state).pickerPage).toBeNull();
    const picked = () => adapter(state).pickerPage?.profiles.map((p) => p.id);
    await c.request({ type: "profiles.page", adapterId: ID, page: "first", picker: true });
    expect(picked()?.[0]).toBe(1);
    await c.request({ type: "profiles.page", adapterId: ID, page: "next", picker: true });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next", picker: true });
    expect(picked()).toEqual([21, 22, 23, 24, 25]);
    expect(adapter(state).pickerPage).toMatchObject({ previous: true, next: false });
    expect(ids(state)[0]).toBe(11);
    expect(page(state)).toMatchObject({ previous: true, next: true });
    // Profile events update both pages.
    await c.act({ type: "profile.delete", adapterId: ID, profile: 25 });
    await until(() => picked()?.join() === "21,22,23,24");
    expect(ids(state)[0]).toBe(11);
    await c.stop();
  });

  it("moves back a page when deleting leaves the shown one empty, and places new profiles", async () => {
    const profiles = Array.from({ length: 11 }, (_, i) => profile(i + 1, `P${i + 1}`));
    const { fake, c, state } = await start({ profiles });
    await c.request({ type: "profiles.page", adapterId: ID, page: "next" });
    expect(ids(state)).toEqual([11]);
    expect(await c.act({ type: "profile.create", adapterId: ID, name: "D" })).toEqual({ ok: true });
    await until(() => ids(state).length === 2);
    expect(ids(state)).toEqual([11, 12]);
    await c.act({ type: "profile.delete", adapterId: ID, profile: 11 });
    await c.act({ type: "profile.delete", adapterId: ID, profile: 12 });
    await until(() => ids(state)[0] === 1);
    expect(page(state)).toMatchObject({ previous: false, next: false });
    // A profile that starts a new page leaves the shown page where it is.
    await c.act({ type: "profile.create", adapterId: ID, name: "E" });
    await until(() => page(state).next);
    expect(ids(state)[0]).toBe(1);
    // Events keep the listing current without listing again.
    expect(commands(fake).filter((x) => x === "listProfiles")).toHaveLength(1);
    await c.stop();
  });

  it("applies profile events that follow a listing to what it listed", async () => {
    const { fake, c, state } = await start({ profiles: [profile(1, "A"), profile(2, "B")], batchSize: 1 });
    const { connection } = c.manager.connected.get(ID)!.session;
    // The listing and the removal's event arrive together, in the adapter's order.
    const release = fake.hold();
    const listed = connection.listProfiles();
    const removed = connection.deleteProfile(2);
    release();
    expect((await listed).entries).toHaveLength(2);
    await removed;
    await until(() => ids(state).join() === "1");
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
    await session.connection.setProfileRules({ profile: 1, changes: [{ change: { case: "rule", value: scale([0x01, 0x38], [0x01, 0x02], 2) } }] });
    await until(() => page(state).profiles[0]!.roles.length === 3);
    expect(page(state).profiles[0]!.roles).toEqual(["keyboard", "mouse", "consumer_control"]);
    expect(fake.profiles[0]!.rules).toHaveLength(3);
    await c.stop();
  });

  it("keys rules by input and collection", async () => {
    const { fake, c, state } = await start({ profiles: [profile(1, "A")], batchSize: 1 });
    const { connection } = c.manager.connected.get(ID)!.session;
    const save = (...rules: ReturnType<typeof remap>[]) => connection.setProfileRules({ profile: 1, changes: rules.map((value) => ({ change: { case: "rule" as const, value } })) });
    const list = async () => (await connection.listProfileRules(1)).rules.map((r) => [r.input?.usagePage, r.input?.usage, r.collection?.usage]);
    // Mouse Button 1 and Game Pad Button 1 share a usage but are different inputs.
    await save(
      remap([0x09, 0x01], [0x01, 0x02], [[0x07, 0x04]]),
      remap([0x09, 0x01], [0x01, 0x05], [[0x09, 0x01]], [0x01, 0x02]),
      axis([0x02, 0xc5], [0x02, 0xc4], true),
    );
    const sent = fake.received.length;
    const { rules: listed } = await connection.listProfileRules(1);
    expect(listed.map((r) => [r.input?.usagePage, r.input?.usage, r.collection?.usage])).toEqual([[0x02, 0xc5, 0x05], [0x09, 0x01, 0x02], [0x09, 0x01, 0x05]]);
    expect(listed[0]!.effect).toMatchObject({ case: "axis", value: { destination: { usagePage: 0x02, usage: 0xc4 }, invert: true } });
    expect(listed[1]!.effect).toMatchObject({ case: "remap", value: { outputs: [{ usage: { usagePage: 0x07, usage: 0x04 }, collection: { usagePage: 0x01, usage: 0x06 } }] } });
    expect(listed[2]!.effect).toMatchObject({ case: "remap", value: { outputs: [{ usage: { usagePage: 0x09, usage: 0x01 }, collection: { usagePage: 0x01, usage: 0x02 } }] } });
    expect(fake.received.slice(sent).map((r) => r.command.case)).toEqual(["listProfileRules"]);
    await until(() => page(state).profiles[0]!.roles.length === 2);
    expect(page(state).profiles[0]!.roles).toEqual(["mouse", "gamepad"]);
    // Forgetting one leaves the other.
    await connection.setProfileRules({ profile: 1, changes: [{ change: { case: "forget", value: { input: { usagePage: 0x09, usage: 0x01 }, collection: { usagePage: 0x01, usage: 0x05 } } } }] });
    expect(await list()).toEqual([[0x02, 0xc5, 0x05], [0x09, 0x01, 0x02]]);
    // Inputs and outputs need their capability kind and collection.
    for (const bad of [
      remap([0x01, 0x30], [0x01, 0x02], []),
      scale([0x07, 0x04], [0x01, 0x06], 2),
      remap([0x07, 0x04], [0x01, 0x06], [[0x01, 0x30]], [0x01, 0x02]),
      axis([0x01, 0x3d], [0x01, 0x30]),
      axis([0x01, 0x30], [0x01, 0x3d]),
      remap([0x09, 0x01], [0x0c, 0x01], []),
      remap([0x09, 0x21], [0x01, 0x05], []),
      remap([0x07, 0x04], [0x01, 0x06], [[0x09, 0x01]]),
      create(ProfileRuleSchema, { input: { usagePage: 0x09, usage: 0x01 }, effect: { case: "remap", value: {} } }),
    ]) {
      await expect(save(bad)).rejects.toMatchObject({ code: ErrorCode.BAD_ARGS });
    }
    expect(await list()).toEqual([[0x02, 0xc5, 0x05], [0x09, 0x01, 0x02]]);
    await c.stop();
  });

  it("remaps Game Pad input without gamepad outputs", async () => {
    const { c } = await start({ board: "xiao_esp32s3", profiles: [profile(1, "A")] });
    const { connection } = c.manager.connected.get(ID)!.session;
    const support = connection.status.profileSupport!;
    expect(support.ranges.some((r) => r.kind === UsageRange_Kind.REMAP_INPUT && r.collection?.usage === 0x05)).toBe(true);
    expect(support.ranges.some((r) => r.kind === UsageRange_Kind.REMAP_OUTPUT && r.collection?.usage === 0x05)).toBe(false);
    expect(support.ranges.some((r) => r.kind === UsageRange_Kind.AXIS_INPUT || r.kind === UsageRange_Kind.AXIS_OUTPUT)).toBe(false);
    const save = (value: ReturnType<typeof remap>) => connection.setProfileRules({ profile: 1, changes: [{ change: { case: "rule", value } }] });
    // Only the Mouse collection sends Button 1 here.
    await save(remap([0x09, 0x01], [0x01, 0x05], [[0x07, 0x04], [0x09, 0x01]]));
    const [rule] = (await connection.listProfileRules(1)).rules;
    expect(rule!.effect.value).toMatchObject({ outputs: [{ collection: { usage: 0x06 } }, { collection: { usage: 0x02 } }] });
    for (const bad of [remap([0x07, 0x04], [0x01, 0x06], [[0x09, 0x01]], [0x01, 0x05]), axis([0x01, 0x30], [0x01, 0x31])]) {
      await expect(save(bad)).rejects.toMatchObject({ code: ErrorCode.BAD_ARGS });
    }
    await c.stop();
  });

  it("names the profiles in layers and interfaces when the profiles can't be listed", async () => {
    const profiles = [1, 2, 3].map((id) => profile(id, `P${id}`, ["keyboard"]));
    const fake = new FakeAdapter({
      adapterId: ID,
      devices: [device(1, { profiles: [3, 9] })],
      profiles,
      interfaces: { vial: { enabled: false, profile: 2 } },
    });
    fake.failures.listProfiles = [ErrorCode.BUSY];
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
    expect(await del()).toEqual({ ok: false, message: "This profile is still in use. Remove it from every device and interface before deleting it." });
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
      { interface: 1, enabled: false, active: false, profile: 0, conflicts: [2] },
      { interface: 2, enabled: true, active: true, profile: 1, conflicts: [1] },
    ]);
    // Another profile for the enabled interface leaves the USB interfaces as they are.
    expect(await c.act({ type: "adapter.settings", adapterId: ID, interfaces: { 2: { profile: 2 } } })).toMatchObject({ ok: false });
    await c.act({ type: "profile.create", adapterId: ID, name: "B" });
    await until(() => !!adapter(state).profileNames[2]);
    expect(await c.act({ type: "adapter.settings", adapterId: ID, interfaces: { 2: { profile: 2 } } })).toEqual({ ok: true });
    await until(() => adapter(state).status!.interfaces[1]!.profile === 2);
    expect(fake.usbReconnects).toBe(1);
    expect(adapter(state).connection).toBe("connected");
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
    await until(() => adapter(state).status!.errors.includes("profile_memory_low"));
    await until(() => alerts().length === 1);
    expect(alerts()[0]).toEqual({ kind: "memory", adapterId: ID, name: "Pico 2 W" });
    expect(profileAlertText(alerts()[0]!)).toEqual({ title: "Pico 2 W is almost out of profile memory.", body: null });
    await c.act({ type: "device.update", key: `${ID}/2`, profiles: [2] });
    await until(() => alerts().length === 2);
    expect(alerts()[1]).toEqual({ kind: "device", key: `${ID}/2`, name: "Keyboard", code: "profile_memory_exceeded" });
    expect(state().devices[1]!.device.errors).toEqual(["profile_memory_exceeded"]);
    expect(profileAlertText(alerts()[1]!)).toEqual({
      title: "Keyboard connected without its profiles.",
      body: "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles.",
    });
    // Releasing memory loads the waiting device's profiles and clears both conditions.
    await c.act({ type: "device.update", key: KEY, profiles: [] });
    await until(() => state().devices[1]!.device.errors.length === 0 && adapter(state).status!.errors.length === 0);
    expect(memoryPercent(adapter(state).status)).toBe(20);
    expect(alerts()).toHaveLength(2);
    // Each notifies again once the condition returns.
    await c.act({ type: "device.update", key: KEY, profiles: [1] });
    await until(() => state().devices[0]!.device.errors.includes("profile_memory_exceeded"));
    await until(() => alerts().length === 3);
    expect(alerts()[2]).toMatchObject({ kind: "device", key: KEY });
    fake.changeAdapter({ memoryBudget: 1100 });
    await until(() => alerts().length === 4);
    expect(alerts()[3]).toEqual({ kind: "memory", adapterId: ID, name: "Pico 2 W" });
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
    information: { firmwareVersion: null, boardName: null, developmentBuild: null },
    errors: [],
    resources: [],
    profileSupport: { resources: [{ type: "memory", count: 0, limit: 100 }, { type: "layers", count: null, limit: 4 }] },
    interfaces,
    outputSupport: [],
  });
  const via = { interface: 1, enabled: false, active: false, profile: 0, conflicts: [2] };
  const vial = { interface: 2, enabled: false, active: false, profile: 0, conflicts: [1] };

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
