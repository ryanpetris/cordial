// Exercises the adapter page across a USB reconnect with published state: the controller's
// reconnecting entry (waiting, last status and profiles), a new session without a status, a
// returning adapter that reports its status before it is ready, a failed first device list, the
// ready adapter and a manual disconnect, with staged settings and configuration interfaces saved
// together. Run from desktop:
// xvfb-run -a node test/adapter-ui.mjs
import { _electron } from "playwright";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";

const profile = await mkdtemp(join(tmpdir(), "cordial-adapter-ui-"));
await writeFile(join(profile, "preferences.json"), JSON.stringify({ notifyLowBattery: false, notifyConnections: false }));
const env = { ...process.env, CORDIAL_DESKTOP_SIMULATE: "1" };
delete env.ELECTRON_RUN_AS_NODE;
const app = await _electron.launch({ args: ["--no-sandbox", ".", `--user-data-dir=${profile}`], env });
const page = await app.firstWindow();
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
try {
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1100, 900));
  await page.getByRole("button", { name: /^Pico 2 W/ }).first().waitFor();
  const real = await page.evaluate(() => window.cordial.state());
  const adapter = real.adapters.find((a) => a.name.startsWith("Pico 2 W"));
  assert.ok(adapter.status?.ready && adapter.status.profileSupport);
  const profiles = [{ id: 1, name: "Default", roles: ["keyboard"] }, { id: 2, name: "Gaming", roles: ["mouse", "consumer_control"] }];
  const pageOf = (list, error = null) => ({ profiles: list, unreadable: [], previous: false, next: false, loading: false, error });
  const profilePage = pageOf(profiles);
  const profileNames = Object.fromEntries(profiles.map((p) => [p.id, p]));
  const status = {
    ...adapter.status,
    interfaces: [
      { interface: 1, enabled: true, active: true, profile: 1, conflicts: [2] },
      { interface: 2, enabled: false, active: false, profile: 0, conflicts: [1] },
    ],
  };
  const ready = { ...real, adapters: [{ ...adapter, connection: "connected", readiness: "ready", status, profilePage, pickerPage: profilePage, profileNames, profilesAvailable: true }] };
  // As the controller lists an adapter whose USB went away: its last status, profiles and device
  // records stay, unavailable.
  const retained = {
    ...real,
    devices: real.devices.map((d) => (d.adapterId === adapter.id ? { ...d, available: false } : d)),
    adapters: [{
      ...adapter,
      connection: "reconnecting",
      connectError: null,
      readiness: "waiting",
      status,
      attention: [],
      profilePage: pageOf(profiles, "The adapter stopped responding"),
      pickerPage: null,
      profileNames,
      profilesAvailable: false,
    }],
  };
  // The same session once USB returns, before the adapter is ready.
  const returned = { ...retained, adapters: [{ ...retained.adapters[0], connection: "connected", status: { ...status, ready: false } }] };
  // A new session before the adapter reports its status.
  const starting = { ...retained, adapters: [{ ...adapter, connection: "connected", readiness: "waiting", status: null, attention: [], profilePage: null, pickerPage: null, profileNames: {}, profilesAvailable: false }] };
  const disconnected = { ...retained, adapters: [{ ...adapter, connection: "disconnected", connectError: null, readiness: "waiting", status: null, attention: [], profilePage: null, pickerPage: null, profileNames: {}, profilesAvailable: false }] };

  await app.evaluate(({ ipcMain }, state) => {
    globalThis.uiState = state;
    globalThis.uiActions = [];
    globalThis.uiResult = { ok: true };
    ipcMain.removeHandler("state");
    ipcMain.handle("state", () => globalThis.uiState);
    ipcMain.removeHandler("act");
    ipcMain.handle("act", (_event, action) => {
      globalThis.uiActions.push(action);
      return globalThis.uiResult;
    });
  }, ready);
  const publish = (state) => app.evaluate(({ BrowserWindow }, state) => {
    globalThis.uiState = state;
    BrowserWindow.getAllWindows()[0].webContents.send("state", state);
  }, state);
  const actions = () => app.evaluate(() => globalThis.uiActions);
  const answer = (result) => app.evaluate((_, result) => { globalThis.uiResult = result; }, result);
  await publish(ready);

  const tab = (name) => page.getByRole("tab", { name, exact: true });
  const selected = async (name) => assert.equal(await tab(name).getAttribute("aria-selected"), "true");
  const via = page.getByRole("switch", { name: "VIA", exact: true });
  const vial = page.getByRole("switch", { name: "Vial", exact: true });
  const disabled = (locator, value) => locator.evaluate((e, v) => new Promise((resolve) => {
    const check = () => (e.disabled === v ? resolve() : requestAnimationFrame(check));
    check();
  }), value);
  const dialog = page.getByRole("dialog");
  const bar = page.locator(".page-bar");
  const save = bar.getByRole("button", { name: "Save", exact: true });
  const discard = bar.getByRole("button", { name: "Discard", exact: true });
  const empty = (text) => page.locator(".page-empty").getByRole("heading", { name: text, exact: true });
  const openAdapter = () => page.getByRole("button", { name: /^Pico 2 W/ }).first().click();
  await openAdapter();
  assert.deepEqual(await page.getByRole("tab").allTextContents(), ["Details", "Settings", "Profiles", "Diagnostics"]);

  // Details names each USB capacity problem. Only a shortfall of required capacity also warns in
  // a banner.
  const required = "The adapter doesn't have enough USB capacity for every required interface.";
  const usbFact = page.locator(".fact").filter({ has: page.locator("dt", { hasText: /^Interfaces$/ }) });
  const problem = (code) => ({ code, reason: "unknown", outcome: "unspecified", related: [] });
  status.errors = [problem("usb_required_capacity_exceeded"), problem("usb_reserved_capacity_exceeded")];
  ready.adapters[0].attention = [required];
  await publish(ready);
  await page.locator(".banner").getByText(required, { exact: true }).waitFor();
  await usbFact.getByText("Required Capacity Exceeded", { exact: true }).waitFor();
  await usbFact.getByText("Reserved Capacity Exceeded", { exact: true }).waitFor();
  status.errors = [problem("usb_reserved_capacity_exceeded")];
  ready.adapters[0].attention = [];
  await publish(ready);
  await page.locator(".banner").getByText(required, { exact: true }).waitFor({ state: "detached" });
  await usbFact.getByText("Reserved Capacity Exceeded", { exact: true }).waitFor();
  assert.equal(await usbFact.getByText("Required Capacity Exceeded", { exact: true }).count(), 0);
  status.errors = [];
  await publish(ready);
  await usbFact.getByText("Reserved Capacity Exceeded", { exact: true }).waitFor({ state: "detached" });

  // Diagnostics lists the adapter's errors with every detail, and the saved devices it couldn't
  // read, which Retry lists again.
  status.errors = [{ code: "storage_failed", reason: "storage", outcome: "unknown", related: [] }];
  ready.adapters[0].unreadableDevices = [{ id: 9, error: { code: "storage_failed", reason: "storage", outcome: "no_effect", related: [] } }];
  await publish(ready);
  await tab("Diagnostics").click();
  await page.getByText("The adapter couldn't read or write its saved data. The adapter couldn't confirm whether the change was saved. Check before trying again.", { exact: true }).waitFor();
  await page.getByText("Device 9", { exact: true }).waitFor();
  await page.getByText("Couldn't Read", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  assert.deepEqual(await actions(), [{ type: "devices.reload", adapterId: adapter.id }]);
  await app.evaluate(() => { globalThis.uiActions = []; });
  status.errors = [];
  ready.adapters[0].unreadableDevices = [];
  await publish(ready);
  await page.getByText("Device 9", { exact: true }).waitFor({ state: "detached" });

  // A fresh adapter whose first device list fails needs attention, and Diagnostics names the
  // failure with Retry. Retry lists again as the adapter is starting, and a listing that succeeds
  // leaves the adapter ready. The chosen tab and staged settings stay throughout.
  const pill = page.locator(".page-header .pill");
  const listFailure = page.getByText("The adapter couldn't read its devices. Try again.", { exact: true });
  const retry = page.getByRole("button", { name: "Retry", exact: true });
  const firstList = (listing, listError) => ({
    ...ready,
    devices: ready.devices.filter((d) => d.adapterId !== adapter.id),
    adapters: [{ ...ready.adapters[0], readiness: "waiting", listing, listError, profilePage: null, pickerPage: null, profileNames: {}, profilesAvailable: false }],
  });
  await tab("Settings").click();
  await page.getByRole("radio", { name: "Windows" }).click();
  await page.getByRole("img", { name: "Platform, Changed", exact: true }).waitFor();
  await tab("Diagnostics").click();
  await publish(firstList(true, null));
  await pill.getByText("Starting…", { exact: true }).waitFor();
  assert.equal(await listFailure.count(), 0);
  assert.equal(await retry.count(), 0);
  await publish(firstList(false, "The adapter couldn't read its saved data. Try again."));
  await pill.getByText("Needs Attention", { exact: true }).waitFor();
  await listFailure.waitFor();
  await retry.click();
  assert.deepEqual(await actions(), [{ type: "devices.reload", adapterId: adapter.id }]);
  await app.evaluate(() => { globalThis.uiActions = []; });
  await publish(firstList(true, null));
  await pill.getByText("Starting…", { exact: true }).waitFor();
  await listFailure.waitFor({ state: "detached" });
  assert.equal(await retry.count(), 0);
  await publish(ready);
  await pill.getByText("Ready", { exact: true }).waitFor();
  await selected("Diagnostics");
  assert.equal(await listFailure.count(), 0);
  await tab("Settings").click();
  assert.equal(await page.getByRole("radio", { name: "Windows" }).getAttribute("aria-checked"), "true");
  await discard.click();
  await page.getByRole("img", { name: "Platform, Changed", exact: true }).waitFor({ state: "detached" });
  await tab("Details").click();

  // Each profile shows an icon for each role it changes.
  await tab("Profiles").click();
  await page.getByRole("img", { name: "Media Keys", exact: true }).waitFor();
  assert.equal(await page.getByRole("img", { name: "Mouse", exact: true }).count(), 1);
  assert.equal(await page.getByRole("button", { name: "VIA Profile", exact: true }).textContent(), "Default");
  assert.equal(await page.getByRole("button", { name: "Vial Profile", exact: true }).textContent(), "Choose Profile");
  // Vial can't be turned on without a profile, or while VIA, which it conflicts with, is on.
  assert.equal(await vial.isDisabled(), true);

  // Choosing a value stages it; nothing is sent until Save.
  assert.equal(await save.isDisabled(), true);
  await via.click();
  assert.equal(await dialog.isVisible(), false);
  await page.getByRole("img", { name: "VIA, Changed" }).waitFor();
  assert.deepEqual(await actions(), []);

  // While USB is away, the staged value stays with the controls disabled.
  await publish(retained);
  await disabled(via, true);
  await selected("Profiles");
  assert.equal(await via.isChecked(), false);
  assert.equal(await save.isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "New Profile", exact: true }).isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "VIA Profile", exact: true }).isDisabled(), true);
  for (const b of await page.getByRole("button", { name: /, Options$/ }).all()) assert.equal(await b.isDisabled(), true);

  // The other settings stay shown and disabled too; Details keeps the last status and reports the
  // adapter disconnected.
  await tab("Settings").click();
  assert.equal(await page.getByRole("radio", { name: "Linux" }).isDisabled(), true);
  for (const s of await page.getByRole("switch").all()) assert.equal(await s.isDisabled(), true);
  await tab("Details").click();
  await page.locator(".page-header .pill").getByText("Disconnected", { exact: true }).waitFor();
  await page.getByRole("heading", { name: "Information", exact: true }).waitFor();
  assert.equal(await page.locator(".page-empty").count(), 0);
  await tab("Profiles").click();

  // A new session has no status for a moment: the tab stays chosen without its contents.
  await publish(starting);
  await empty("Starting…").waitFor();
  await selected("Profiles");
  assert.equal(await via.count(), 0);
  // A fault reported while starting needs attention rather than reading as starting, and the
  // status it comes with shows with its controls disabled.
  const fault = { ...status, ready: false, errors: [{ code: "not_ready", reason: "radio", outcome: "unspecified", related: [] }] };
  await publish({ ...starting, adapters: [{ ...starting.adapters[0], status: fault }] });
  await page.locator(".page-header .pill").getByText("Needs Attention", { exact: true }).waitFor();
  await disabled(via, true);
  assert.equal(await page.locator(".page-empty").count(), 0);
  await publish(starting);
  await empty("Starting…").waitFor();

  // A name dialog opened while ready can't submit once USB goes away, nor while the returning
  // adapter isn't ready yet. The dialog keeps the name, and the staged settings stay.
  await publish(ready);
  await via.waitFor();
  await disabled(via, false);
  const transport = { ble: "Bluetooth LE", classic: "Bluetooth Classic" }[status.transports[0].transport];
  const bluetooth = page.getByRole("switch", { name: transport, exact: true });
  const bluetoothStaged = page.getByRole("img", { name: `${transport}, Changed`, exact: true });
  const windows = page.getByRole("radio", { name: "Windows" });
  await tab("Settings").click();
  await windows.click();
  await bluetooth.click();
  await bluetoothStaged.waitFor();
  await tab("Profiles").click();
  await page.getByRole("button", { name: "New Profile", exact: true }).click();
  const create = dialog.getByRole("button", { name: "Create", exact: true });
  const profileName = page.getByRole("textbox", { name: "Profile name" });
  await profileName.fill("Travel");
  assert.equal(await create.isDisabled(), false);
  await publish(retained);
  await disabled(via, true);
  assert.equal(await create.isDisabled(), true);
  await profileName.press("Enter");
  await publish(returned);
  await page.locator(".page-header .pill").getByText("Disconnected", { exact: true }).waitFor({ state: "detached" });
  await selected("Profiles");
  assert.equal(await dialog.isVisible(), true);
  assert.equal(await profileName.inputValue(), "Travel");
  assert.equal(await create.isDisabled(), true);
  assert.equal(await via.isDisabled(), true);
  await publish(ready);
  await disabled(create, false);
  assert.equal(await profileName.inputValue(), "Travel");
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  await publish(returned);
  await disabled(via, true);
  await tab("Settings").click();
  assert.equal(await windows.getAttribute("aria-checked"), "true");
  assert.equal(await windows.isDisabled(), true);
  assert.equal(await bluetooth.isDisabled(), true);
  await bluetoothStaged.waitFor();
  await tab("Details").click();
  await page.getByRole("heading", { name: "Information", exact: true }).waitFor();
  assert.equal(await page.locator(".page-empty").count(), 0);
  await publish(ready);
  await tab("Settings").click();
  await disabled(bluetooth, false);
  await bluetooth.click();
  await bluetoothStaged.waitFor({ state: "detached" });
  await tab("Profiles").click();
  assert.deepEqual(await actions(), []);

  // Staged edits on both tabs survive visiting another page and go out together. With VIA staged
  // off, Vial can be turned on once it has a profile.
  await publish(ready);
  await disabled(via, false);
  assert.equal(await via.isChecked(), false);
  await page.getByRole("button", { name: "Vial Profile", exact: true }).click();
  const picker = page.getByRole("dialog", { name: "Vial Profile" });
  await picker.getByRole("radio", { name: "Gaming", exact: true }).check();
  await picker.getByRole("button", { name: "Choose", exact: true }).click();
  await picker.waitFor({ state: "hidden" });
  // The picker reads its own first page when it opens.
  const pick = { type: "profiles.page", adapterId: adapter.id, page: "first", picker: true };
  await disabled(vial, false);
  await vial.click();
  await page.getByRole("img", { name: "Vial, Changed" }).waitFor();
  await tab("Settings").click();
  await page.getByRole("radio", { name: "Windows" }).click();
  const other = real.devices.find((d) => d.adapterId === adapter.id);
  if (other) {
    await page.getByRole("button", { name: other.name }).first().click();
    await openAdapter();
    await tab("Settings").click();
  }
  assert.equal(await page.getByRole("radio", { name: "Windows" }).getAttribute("aria-checked"), "true");

  // A ready adapter that hasn't read its profiles since reconnecting can't save the staged
  // interfaces until a fresh read succeeds. The other settings and Retry stay available.
  await publish({ ...ready, adapters: [{ ...ready.adapters[0], profilePage: pageOf(profiles, "The adapter stopped responding"), pickerPage: null, profilesAvailable: false }] });
  await disabled(save, true);
  assert.equal(await page.getByRole("radio", { name: "Windows" }).isDisabled(), false);
  assert.equal(await page.getByRole("radio", { name: "Windows" }).getAttribute("aria-checked"), "true");
  assert.equal(await discard.isDisabled(), false);
  await tab("Profiles").click();
  await page.getByRole("img", { name: "Vial, Changed" }).waitFor();
  assert.equal(await vial.isChecked(), true);
  assert.equal(await save.isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).isDisabled(), false);
  await publish(ready);
  await disabled(save, false);
  assert.equal(await vial.isChecked(), true);
  assert.equal(await via.isChecked(), false);
  await tab("Settings").click();
  assert.deepEqual(await actions(), [pick]);

  // A failed save keeps the staged values.
  await answer({ ok: false, message: "The adapter couldn't save the change." });
  await save.click();
  assert.equal(await dialog.isVisible(), false);
  await bar.getByText("The adapter couldn't save the change.").waitFor();
  await tab("Profiles").click();
  assert.equal(await via.isChecked(), false);
  assert.equal(await vial.isChecked(), true);
  const change = {
    type: "adapter.settings",
    adapterId: adapter.id,
    platform: "windows",
    interfaces: { 1: { enabled: false }, 2: { enabled: true, profile: 2 } },
  };
  assert.deepEqual(await actions(), [pick, change]);

  // Saving sends at once, without asking first, even though it reconnects USB.
  await answer({ ok: true });
  await save.click();
  await disabled(save, true);
  for (const name of ["VIA, Changed", "Vial, Changed"]) await page.getByRole("img", { name }).waitFor({ state: "detached" });
  assert.equal(await dialog.isVisible(), false);
  assert.deepEqual(await actions(), [pick, change, change]);
  assert.equal(await via.isChecked(), true);
  assert.equal(await vial.isChecked(), false);

  // Discard drops staged values without sending anything.
  await via.click();
  await discard.click();
  assert.equal(await via.isChecked(), true);
  assert.deepEqual(await actions(), [pick, change, change]);

  // A profile's menu opens its dialogs; creating, copying and deleting act at once.
  await page.getByRole("button", { name: "Gaming, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Delete", exact: true }).click();
  await dialog.getByRole("button", { name: "Delete", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  assert.deepEqual(await actions(), [pick, change, change, { type: "profile.delete", adapterId: adapter.id, profile: 2 }]);

  // A manual disconnect keeps nothing but the chosen tab.
  await publish(disconnected);
  await empty("Disconnected").waitFor();
  await selected("Profiles");
  assert.equal(await via.count(), 0);
  assert.deepEqual(errors, []);
  console.log("Adapter UI: tabs, staged settings and interfaces, retained settings, disabled dialog writes and reconnect states passed");
} catch (e) {
  console.error(await page.locator("body").innerText());
  throw e;
} finally {
  await app.close();
  await rm(profile, { recursive: true, force: true });
}
