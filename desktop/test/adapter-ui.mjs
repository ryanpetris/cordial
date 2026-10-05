// Exercises the adapter page across a USB reconnect with published state: the controller's
// retained entry (connecting, waiting, last status and profiles), a new session without a status,
// the ready adapter and a manual disconnect, with staged settings and configuration interfaces
// saved together. Run from desktop: xvfb-run -a node test/adapter-ui.mjs
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
      { interface: 1, enabled: true, profile: 1, conflicts: [2] },
      { interface: 2, enabled: false, profile: 0, conflicts: [1] },
    ],
  };
  const ready = { ...real, adapters: [{ ...adapter, connection: "connected", readiness: "ready", status, profilePage, pickerPage: profilePage, profileNames }] };
  // As the controller lists an adapter whose USB went away: its last status and profiles stay.
  const retained = {
    ...real,
    devices: real.devices.filter((d) => d.adapterId !== adapter.id),
    adapters: [{
      ...adapter,
      connection: "connecting",
      connectError: null,
      readiness: "waiting",
      status,
      attention: [],
      profilePage: pageOf(profiles, "The adapter stopped responding"),
      pickerPage: null,
      profileNames,
    }],
  };
  // A new session before the adapter reports its status.
  const starting = { ...retained, adapters: [{ ...adapter, connection: "connected", readiness: "waiting", status: null, attention: [], profilePage: null, pickerPage: null, profileNames: {} }] };
  const disconnected = { ...retained, adapters: [{ ...adapter, connection: "disconnected", connectError: null, readiness: "waiting", status: null, attention: [], profilePage: null, pickerPage: null, profileNames: {} }] };

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

  // Save asks before reconnecting USB. A confirmation left open when USB goes away can't be
  // confirmed; the staged value stays.
  await save.click();
  const confirm = dialog.getByRole("button", { name: "Save", exact: true });
  await dialog.getByText("The adapter will disconnect from this computer for a moment after it saves these changes.").waitFor();
  assert.equal(await confirm.isDisabled(), false);
  await publish(retained);
  await disabled(via, true);
  assert.equal(await confirm.isDisabled(), true);
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  await selected("Profiles");
  assert.equal(await via.isChecked(), false);
  assert.equal(await save.isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "New Profile", exact: true }).isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: "VIA Profile", exact: true }).isDisabled(), true);
  for (const b of await page.getByRole("button", { name: /, Options$/ }).all()) assert.equal(await b.isDisabled(), true);

  // The other settings stay shown and disabled too; Details reports the reconnect.
  await tab("Settings").click();
  assert.equal(await page.getByRole("radio", { name: "Linux" }).isDisabled(), true);
  for (const s of await page.getByRole("switch").all()) assert.equal(await s.isDisabled(), true);
  await tab("Details").click();
  await empty("Connecting…").waitFor();
  await tab("Profiles").click();

  // A new session has no status for a moment: the tab stays chosen without its contents.
  await publish(starting);
  await empty("Starting…").waitFor();
  await selected("Profiles");
  assert.equal(await via.count(), 0);

  // A name dialog opened while ready can't submit once USB goes away.
  await publish(ready);
  await via.waitFor();
  await disabled(via, false);
  await page.getByRole("button", { name: "New Profile", exact: true }).click();
  const create = dialog.getByRole("button", { name: "Create", exact: true });
  await page.getByRole("textbox", { name: "Profile name" }).fill("Travel");
  assert.equal(await create.isDisabled(), false);
  await publish(retained);
  await disabled(via, true);
  assert.equal(await create.isDisabled(), true);
  await page.getByRole("textbox", { name: "Profile name" }).press("Enter");
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
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

  // A failed save keeps the staged values.
  await answer({ ok: false, message: "The adapter couldn't save the change." });
  await save.click();
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
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

  await answer({ ok: true });
  await save.click();
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await disabled(save, true);
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
