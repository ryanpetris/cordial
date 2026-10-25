// Exercises the window while an adapter's USB goes away unexpectedly, with published state: the
// controller keeps the adapter, reconnecting, and its devices' records, unavailable, until the
// adapter returns or its reconnect window ends. Pages, tabs, staged edits and the Add Device
// dialog stay with their actions disabled; a manual disconnect stays distinct. Run from desktop:
// xvfb-run -a node test/reconnect-ui.mjs. CORDIAL_EXECUTABLE runs an installed build instead.
import { _electron } from "playwright";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";

// A separate profile with notifications off, as in the smoke run.
const profile = await mkdtemp(join(tmpdir(), "cordial-reconnect-ui-"));
await writeFile(join(profile, "preferences.json"), JSON.stringify({ notifyLowBattery: false, notifyConnections: false }));
const env = { ...process.env, CORDIAL_DESKTOP_SIMULATE: "2" };
delete env.ELECTRON_RUN_AS_NODE;
const app = await _electron.launch({
  executablePath: process.env.CORDIAL_EXECUTABLE,
  chromiumSandbox: true,
  args: [...(process.env.CORDIAL_EXECUTABLE ? [] : ["."]), `--user-data-dir=${profile}`],
  env,
});
const page = await app.firstWindow();
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
try {
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1100, 900));
  await page.getByRole("button", { name: /^Pico 2 W/ }).first().waitFor();
  const real = await page.evaluate(() => window.cordial.state());
  const adapter = real.adapters.find((a) => a.name.startsWith("Pico 2 W"));
  const other = real.adapters.find((a) => a !== adapter);
  assert.ok(adapter.status?.ready && other?.readiness === "ready");
  const keys = real.devices.find((d) => d.adapterId === adapter.id && d.name.startsWith("Example Keys"));
  assert.ok(keys.available && keys.device.state === "connected" && keys.battery?.percent != null);
  const otherDevice = real.devices.find((d) => d.adapterId === other.id);
  const clone = (s) => structuredClone(s);
  const candidate = { id: 7, transport: "ble", name: "Travel Keys", kinds: ["keyboard"], rssi: -50 };

  // As the controller publishes the adapter while it reconnects: the last status and records stay,
  // nothing it reported is current, and its search has ended.
  const lose = (s) => ({
    ...s,
    scan: null,
    adapters: s.adapters.map((a) => (a.id === adapter.id ? { ...a, connection: "reconnecting", readiness: "waiting", profilesAvailable: false } : a)),
    devices: s.devices.map((d) => (d.adapterId === adapter.id
      ? { ...d, available: false, settingsAvailable: false, battery: d.battery && { ...d.battery, percentFresh: false, chargingFresh: false } }
      : d)),
  });
  // The reconnect window ended: the adapter and its devices are gone.
  const expire = (s) => ({
    ...s,
    scan: null,
    pairing: null,
    adapters: s.adapters.filter((a) => a.id !== adapter.id),
    devices: s.devices.filter((d) => d.adapterId !== adapter.id),
  });
  const ready = clone(real);

  await app.evaluate(({ ipcMain }, state) => {
    globalThis.uiState = state;
    globalThis.uiActions = [];
    ipcMain.removeHandler("state");
    ipcMain.handle("state", () => globalThis.uiState);
    ipcMain.removeHandler("act");
    ipcMain.handle("act", (_event, action) => {
      globalThis.uiActions.push(action);
      return { ok: true };
    });
  }, ready);
  const publish = (state) => app.evaluate(({ BrowserWindow }, state) => {
    globalThis.uiState = state;
    BrowserWindow.getAllWindows()[0].webContents.send("state", state);
  }, state);
  const actions = () => app.evaluate(() => globalThis.uiActions);
  const until = async (test) => {
    for (let i = 0; i < 200 && !(await test()); i++) await page.waitForTimeout(10);
  };
  const clear = () => app.evaluate(() => { globalThis.uiActions = []; });
  await publish(ready);

  const tab = (name) => page.getByRole("tab", { name, exact: true });
  const selected = async (name) => assert.equal(await tab(name).getAttribute("aria-selected"), "true");
  const bar = page.locator(".page-bar");
  const barButton = (name) => bar.getByRole("button", { name, exact: true });
  const pill = page.locator(".page-header .pill").first();
  const sidebar = page.getByRole("navigation", { name: "Devices and adapters" });
  const row = (name) => sidebar.getByRole("button", { name: new RegExp(`^${name}`) }).first();
  const empty = page.locator(".page-empty");
  const enabledness = (locator, disabled) => locator.evaluate((e, v) => new Promise((resolve) => {
    const check = () => (e.disabled === v ? resolve() : requestAnimationFrame(check));
    check();
  }), disabled);
  const automatic = page.getByRole("switch", { name: "Automatic Connections", exact: true });
  const platform = (name) => page.getByRole("radio", { name, exact: true });

  // Staged edits on the device and the adapter, and the chosen tabs.
  await row(keys.name).click();
  await automatic.click();
  await page.getByRole("img", { name: "Automatic Connections, Changed" }).waitFor();
  const staged = !keys.device.trusted;
  await row("Pico 2 W").click();
  await tab("Settings").click();
  const target = adapter.status.platform === "windows" ? "Linux" : "Windows";
  await platform(target).click();
  await page.getByRole("img", { name: "Platform, Changed" }).waitFor();
  await row(keys.name).click();
  await tab("Diagnostics").click();
  assert.deepEqual(await actions(), []);

  // USB goes away: the device page stays on its tab with its actions disabled, and the sidebar
  // keeps its rows, reporting the adapter and device disconnected.
  const lost = lose(ready);
  await publish(lost);
  await pill.getByText("Disconnected", { exact: true }).waitFor();
  await selected("Diagnostics");
  await enabledness(barButton("Refresh"), true);
  await tab("Details").click();
  assert.equal(await automatic.isChecked(), staged);
  assert.equal(await automatic.isDisabled(), true);
  await page.getByRole("img", { name: "Automatic Connections, Changed" }).waitFor();
  for (const name of ["Forget Device", "Save"]) assert.equal(await barButton(name).isDisabled(), true);
  const connection = bar.getByRole("button", { name: /^(Connect|Disconnect)$/ });
  assert.equal(await connection.isDisabled(), true);
  assert.equal(await page.locator(".page-header .pill.dim").count(), 1);
  await row(keys.name).getByText("Disconnected", { exact: true }).waitFor();
  assert.equal(await row(keys.name).locator(".side-icon.on").count(), 0);
  assert.equal(await row(keys.name).locator(".side-battery.dim").count(), 1);
  await row("Pico 2 W").getByText("Disconnected", { exact: true }).waitFor();
  for (const d of real.devices) await row(d.name).waitFor();
  // Add Device waits while no adapter is ready.
  const addDevice = sidebar.getByRole("button", { name: "Add Device", exact: true });
  await publish({ ...lost, adapters: lost.adapters.map((a) => ({ ...a, connection: "reconnecting", readiness: "waiting" })) });
  await enabledness(addDevice, true);
  await publish(lost);
  await enabledness(addDevice, false);

  // The overview keeps the device's tile without counting it as connected.
  await sidebar.getByRole("button", { name: "Overview", exact: true }).click();
  const tiles = page.locator("section.group").filter({ has: page.getByRole("heading", { name: "Devices", exact: true }) });
  const tile = tiles.locator(".tile").filter({ hasText: keys.name });
  await tile.waitFor();
  assert.equal(await tile.evaluate((e) => e.classList.contains("off")), true);
  const live = real.devices.filter((d) => d.adapterId !== adapter.id && d.device.state === "connected" && !d.readError).length;
  const count = page.locator(".summary-item").filter({ hasText: /^\d+Connected$/ }).locator(".summary-value");
  assert.equal(await count.textContent(), String(live));
  await page.locator(".row").filter({ hasText: adapter.name }).getByText("Disconnected", { exact: true }).waitFor();

  // The adapter page shows its last details and settings with its controls disabled, still staged.
  await row("Pico 2 W").click();
  await pill.getByText("Disconnected", { exact: true }).waitFor();
  await tab("Settings").click();
  assert.equal(await platform(target).getAttribute("aria-checked"), "true");
  assert.equal(await platform(target).isDisabled(), true);
  assert.equal(await barButton("Save").isDisabled(), true);
  await tab("Details").click();
  assert.equal(await empty.count(), 0);
  await page.getByRole("heading", { name: "Information", exact: true }).waitFor();
  await page.locator("section.group").filter({ has: page.getByRole("heading", { name: "Devices", exact: true }) }).getByText(keys.name).waitFor();
  assert.equal(await barButton("Rename").isDisabled(), true);
  assert.equal(await bar.getByRole("button", { name: /^(Connect|Disconnect)$/ }).count(), 0);
  await tab("Settings").click();
  assert.deepEqual(await actions(), []);

  // USB comes back: the same pages show the records the adapter lists again, actions return, and
  // the staged edits are still there.
  const back = clone(ready);
  back.devices.find((d) => d.key === keys.key).battery.percent = keys.battery.percent - 1;
  await publish(back);
  await pill.getByText("Ready", { exact: true }).waitFor();
  await selected("Settings");
  await enabledness(platform(target), false);
  assert.equal(await platform(target).getAttribute("aria-checked"), "true");
  assert.equal(await barButton("Save").isDisabled(), false);
  await row(keys.name).click();
  await selected("Details");
  await enabledness(automatic, false);
  assert.equal(await automatic.isChecked(), staged);
  await row(keys.name).getByText(`${keys.battery.percent - 1}%`).waitFor();
  assert.equal(await row(keys.name).locator(".side-battery.dim").count(), 0);
  assert.equal(await barButton("Save").isDisabled(), false);

  // Add Device keeps the adapter chosen while it reconnects, with searching and pairing disabled,
  // and searches again once it is back.
  await clear();
  await addDevice.click();
  const dialog = page.getByRole("dialog", { name: "Add Device" });
  await dialog.waitFor();
  const chooser = dialog.getByRole("combobox", { name: "Adapter", exact: true });
  await chooser.selectOption(adapter.id);
  const scanning = { ...back, scan: { adapterId: adapter.id, running: false, candidates: [candidate], error: null, failure: null } };
  await publish(scanning);
  const pair = dialog.getByRole("listitem").filter({ hasText: candidate.name }).getByRole("button", { name: "Pair", exact: true });
  await enabledness(pair, false);
  await clear();
  await publish(lose(scanning));
  await pair.waitFor({ state: "detached" });
  assert.equal(await dialog.isVisible(), true);
  assert.equal(await chooser.inputValue(), adapter.id);
  assert.equal(await dialog.getByRole("button", { name: "Refresh", exact: true }).isDisabled(), true);
  assert.deepEqual(await actions(), []);
  await publish(back);
  await until(async () => (await actions()).length > 0);
  assert.deepEqual(await actions(), [{ type: "scan.start", adapterId: adapter.id }]);

  // A saved pairing's result stays while the adapter reconnects; Add Another waits for it, and
  // Done opens the device.
  const paired = { ...back, pairing: { adapterId: adapter.id, candidateId: candidate.id, name: candidate.name, phase: "saved", prompt: null, deviceKey: keys.key, message: "It will connect when it's next in range.", failure: null } };
  await publish(lose(paired));
  await dialog.getByText(`${candidate.name} is paired.`, { exact: true }).waitFor();
  assert.equal(await dialog.getByRole("button", { name: "Add Another", exact: true }).isDisabled(), true);
  await publish(paired);
  await enabledness(dialog.getByRole("button", { name: "Add Another", exact: true }), false);
  await publish(lose(paired));
  await dialog.getByRole("button", { name: "Done", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  await selected("Details");
  await page.locator(".page-header").getByText(keys.name, { exact: true }).waitFor();

  // The reconnect window ends: the adapter's pages and rows go, and an open Add Device closes.
  await publish(back);
  await addDevice.click();
  await dialog.waitFor();
  await chooser.selectOption(adapter.id);
  await publish(lose(back));
  assert.equal(await dialog.isVisible(), true);
  await publish(expire(back));
  await dialog.waitFor({ state: "hidden" });
  await row(keys.name).waitFor({ state: "detached" });
  await page.getByRole("heading", { name: "Overview", exact: true }).waitFor();
  await row(otherDevice.name).waitFor();

  // An adapter that returns after that is listed afresh.
  await publish(back);
  await row(keys.name).waitFor();

  // A manual disconnect shows the adapter without its last status or devices.
  const disconnected = expire(back);
  disconnected.adapters.push({ ...adapter, connection: "disconnected", readiness: "waiting", status: null, listing: false, listError: null, attention: [], profilePage: null, pickerPage: null, profileNames: {}, profilesAvailable: false, unreadableDevices: [] });
  await row("Pico 2 W").click();
  await publish(disconnected);
  await empty.getByRole("heading", { name: "Disconnected", exact: true }).waitFor();
  await barButton("Connect").waitFor();
  assert.equal(await barButton("Connect").isDisabled(), false);
  assert.deepEqual(errors, []);
  console.log("Reconnect UI: pages, tabs, staged edits, Add Device, expiry, return and manual disconnect passed");
} catch (e) {
  console.error(await page.locator("body").innerText());
  throw e;
} finally {
  await app.close();
  await rm(profile, { recursive: true, force: true });
}
