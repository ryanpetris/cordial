// Exercises the built desktop renderer with simulated state and controlled replies.
// Run from desktop: xvfb-run -a node test/device-ui.mjs
import { _electron } from "playwright";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";

const profile = await mkdtemp(join(tmpdir(), "cordial-device-ui-"));
await writeFile(join(profile, "preferences.json"), JSON.stringify({ notifyLowBattery: false, notifyConnections: false }));
const env = { ...process.env, CORDIAL_DESKTOP_SIMULATE: "1" };
delete env.ELECTRON_RUN_AS_NODE;
const app = await _electron.launch({ args: ["--no-sandbox", ".", `--user-data-dir=${profile}`], env });
const page = await app.firstWindow();
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
try {
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1100, 900));
  await page.getByRole("button", { name: /Example Keys/ }).first().click();
  const tab = (name) => page.getByRole("tab", { name, exact: true });
  const bar = page.locator(".page-bar");

  // A device opens on Details, the first tab, whose bar holds Forget Device
  // and the connection button; the header holds no buttons.
  await tab("Details").waitFor();
  assert.deepEqual(await page.getByRole("tab").allTextContents(), ["Details", "Settings", "Profiles", "Diagnostics"]);
  assert.equal(await tab("Details").getAttribute("aria-selected"), "true");
  await bar.getByRole("button", { name: "Forget Device", exact: true }).waitFor();
  await bar.getByRole("button", { name: "Disconnect", exact: true }).waitFor();
  assert.equal(await page.locator(".page-header button").count(), 0);
  await tab("Settings").click();
  await page.getByRole("group", { name: "Backlight", exact: true }).waitFor();
  assert.equal(await bar.getByRole("button", { name: "Save", exact: true }).count(), 1);

  const state = await page.evaluate(() => window.cordial.state());
  const entry = state.devices.find((d) => d.name === "Example Keys Wireless");
  assert.ok(entry?.settings?.length);
  const row = (key) => entry.settings.find((s) => s.key === key);
  const enabled = row("backlight.enabled");
  Object.assign(enabled, { value: null, saved: null, state: null });
  entry.settings.push({ ...enabled, key: "wheel.threshold", type: "integer", min: 1, max: 255, step: 1, value: 30 });
  await app.evaluate(({ ipcMain }, state) => {
    globalThis.uiState = state;
    globalThis.uiActions = [];
    ipcMain.removeHandler("state");
    ipcMain.handle("state", () => globalThis.uiState);
    ipcMain.removeHandler("act");
    ipcMain.handle("act", (_event, action) => {
      globalThis.uiActions.push(action);
      if (globalThis.uiHold === action.type)
        return new Promise((resolve) => { globalThis.uiReply = resolve; });
      if (globalThis.uiFail === action.type) return { ok: false, message: "Couldn't save this value." };
      return { ok: true };
    });
  }, state);
  const publish = async () => app.evaluate(({ BrowserWindow }, state) => {
    globalThis.uiState = state;
    BrowserWindow.getAllWindows()[0].webContents.send("state", state);
  }, state);
  const actions = () => app.evaluate(() => globalThis.uiActions);
  const fail = (type) => app.evaluate((_electron, type) => { globalThis.uiFail = type; }, type);
  await publish();

  const save = page.getByRole("button", { name: "Save", exact: true });
  const discard = page.getByRole("button", { name: "Discard", exact: true });
  const refresh = page.getByRole("button", { name: "Refresh", exact: true });
  const retryButton = page.getByRole("button", { name: "Retry", exact: true });
  const saves = async () => (await actions()).filter((a) => a.type === "settings.save");

  // A failed Refresh reports beside the footer and keeps its name.
  await fail("device.refresh");
  await refresh.click();
  await page.getByText("Couldn't save this value.", { exact: true }).waitFor();
  assert.ok((await actions()).some((a) => a.type === "device.refresh" && a.key === entry.key));
  assert.equal(await page.locator(".toast").count(), 0);
  assert.equal(await retryButton.count(), 0);
  await fail(null);

  // The footer is always there; with nothing staged it has nothing to do.
  // An unknown reading is a mixed checkbox rather than a switch.
  const backlight = page.locator('input.switch[aria-label="Backlight"]');
  assert.equal(await backlight.getAttribute("role"), null);
  assert.equal(await backlight.evaluate((e) => e.indeterminate), true);
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  assert.equal(await discard.getAttribute("aria-disabled"), "true");
  await page.getByRole("button", { name: "Backlight, Not Saved, Options", exact: true }).waitFor();
  await page.getByRole("button", { name: "Timeout With Hands Away, Changed on Device, Options", exact: true }).waitFor();
  await page.getByText("Device: 60", { exact: true }).waitFor();
  // A value the device only reports sits with the settings it belongs to.
  await page.getByRole("group", { name: "Current Backlight Level", exact: true }).getByText("3", { exact: true }).waitFor();

  // Values equal to what the device keeps are no change: a typed 030 against
  // a saved 30, and choosing the reading of an unsaved setting again.
  const hands = page.getByRole("spinbutton", { name: "Timeout With Hands Away" });
  await hands.fill("030");
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  assert.equal(await discard.getAttribute("aria-disabled"), "true");
  // It leaves no draft behind to show once the saved value changes.
  const handsSaved = row("backlight.delay.hands_out");
  handsSaved.saved = 45;
  await publish();
  assert.equal(await hands.inputValue(), "45");
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  handsSaved.saved = 30;
  await publish();
  await hands.press("Escape");

  // SmartShift's threshold stays in place while its text is empty or invalid;
  // Off is its own value, and On again starts from the reading.
  const smart = page.locator('input.switch[aria-label="SmartShift"]');
  const threshold = page.getByRole("spinbutton", { name: "SmartShift Threshold", exact: true });
  assert.equal(await smart.getAttribute("role"), "switch");
  assert.equal(await smart.isChecked(), true);
  await threshold.fill("");
  assert.equal(await threshold.count(), 1);
  assert.equal(await threshold.evaluate((e) => e === document.activeElement), true);
  assert.equal(await smart.isChecked(), true);
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  assert.equal(await discard.getAttribute("aria-disabled"), "false");
  await threshold.pressSequentially("40");
  assert.equal(await threshold.inputValue(), "40");
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  // A typed 255 is out of the threshold's range, not Off: it stays On and invalid.
  await threshold.fill("255");
  assert.equal(await smart.isChecked(), true);
  assert.equal(await threshold.getAttribute("aria-invalid"), "true");
  await page.getByText("1-254", { exact: true }).waitFor();
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  // Only the toggle turns it Off, staging 255.
  await smart.click();
  assert.equal(await smart.isChecked(), false);
  assert.equal(await threshold.count(), 0);
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  await smart.click();
  assert.equal(await threshold.inputValue(), "30");
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  const mode = page.getByRole("combobox", { name: "Backlight Mode", exact: true });
  await mode.selectOption("permanent_manual");
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  await mode.selectOption("automatic");
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  assert.equal(await discard.getAttribute("aria-disabled"), "true");

  // Choosing stages a change; nothing is sent until Save. An unknown value turns On first.
  await backlight.click();
  assert.equal(await backlight.isChecked(), true);
  await backlight.click();
  assert.equal(await backlight.getAttribute("role"), "switch");
  assert.equal(await backlight.isChecked(), false);
  assert.equal((await saves()).length, 0);
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  await page.getByRole("button", { name: "Backlight, Changed, Options", exact: true }).waitFor();

  // Drafts survive tabs and other pages; a page opened again starts on Details.
  await tab("Details").click();
  await tab("Settings").click();
  await page.getByRole("button", { name: /Example Mouse/ }).first().click();
  await page.getByRole("button", { name: /Example Keys/ }).first().click();
  assert.equal(await tab("Details").getAttribute("aria-selected"), "true");
  await tab("Settings").click();
  assert.equal(await backlight.isChecked(), false);

  // A device with no settings to show has no Settings tab.
  await page.getByRole("button", { name: /Travel Keyboard/ }).first().click();
  await tab("Settings").waitFor({ state: "detached" });
  assert.deepEqual(await page.getByRole("tab").allTextContents(), ["Details", "Profiles", "Diagnostics"]);
  await bar.getByRole("button", { name: "Forget Device", exact: true }).waitFor();
  await bar.getByRole("button", { name: "Connect", exact: true }).waitFor();
  await page.getByRole("button", { name: /Example Keys/ }).first().click();
  await tab("Settings").click();

  // Escape undoes only the typed row; Enter submits every staged change in catalog order.
  assert.equal(await hands.inputValue(), "30");
  await hands.fill("45");
  await hands.press("Escape");
  assert.equal(await hands.inputValue(), "30");
  assert.equal(await backlight.isChecked(), false);
  await hands.fill("47");
  await page.getByText("5-7200, Steps of 5", { exact: true }).waitFor();
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  await hands.fill("45");
  await fail("settings.save");
  await hands.press("Enter");
  await page.getByText("Couldn't save this value.", { exact: true }).waitFor();
  assert.equal(await page.locator(".toast").count(), 0);
  assert.deepEqual((await saves()).at(-1), {
    type: "settings.save",
    key: entry.key,
    changes: [
      { type: "set", setting: "backlight.enabled", value: false },
      { type: "set", setting: "backlight.delay.hands_out", value: 45 },
    ],
  });
  // A rejected submission keeps its drafts.
  assert.equal(await save.getAttribute("aria-disabled"), "false");

  // Marker menus stage policy changes: forgetting, and saving what the device shows.
  await page.getByRole("button", { name: "Timeout With Hands Away, Changed, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Undo Change", exact: true }).click();
  assert.equal(await hands.inputValue(), "30");
  await page.getByRole("button", { name: "Timeout With Hands Away, Changed on Device, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Save Device Value", exact: true }).click();
  await page.getByRole("button", { name: "Timeout With Hands Away, Changed, Options", exact: true }).waitFor();
  await page.getByRole("button", { name: "Backlight, Changed, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Undo Change", exact: true }).click();

  // While a submission runs, the form stays visible but can't be edited, and a
  // focused Save keeps focus. Ctrl+S submits.
  await app.evaluate(() => { globalThis.uiFail = null; globalThis.uiHold = "settings.save"; });
  await save.focus();
  await page.keyboard.press("Control+s");
  assert.deepEqual((await saves()).at(-1).changes, [{ type: "set", setting: "backlight.delay.hands_out", value: 60 }]);
  const running = { running: true, items: [{ change: { type: "set", setting: "backlight.delay.hands_out", value: 60 }, status: "saving", error: null }] };
  entry.settingsSave = running;
  await publish();
  assert.equal(await page.evaluate(() => document.activeElement?.textContent?.trim()), "Save");
  assert.equal(await refresh.getAttribute("aria-disabled"), "true");
  assert.equal(await discard.getAttribute("aria-disabled"), "true");
  assert.equal(await backlight.isDisabled(), true);
  await page.getByRole("img", { name: "Timeout With Hands Away, Sending", exact: true }).waitFor();
  assert.equal(await page.locator("form[aria-busy=true]").count(), 1);
  // Logitech Features waits for the settings work too.
  await tab("Details").click();
  assert.equal(await page.getByRole("switch", { name: "Logitech Features" }).isDisabled(), true);
  await tab("Settings").click();

  // Saved, and the apply failed: the draft ends, the setting shows the
  // failure, and Retry saves that value again.
  const outcome = { running: false, items: [{ change: running.items[0].change, status: "saved", error: null }] };
  Object.assign(handsSaved, { saved: 60, state: null, error: "timeout" });
  entry.settingsSave = outcome;
  await publish();
  await app.evaluate((_electron, outcome) => { globalThis.uiReply({ ok: true, settingsSave: outcome }); globalThis.uiHold = null; }, outcome);
  await page.getByRole("button", { name: "Timeout With Hands Away, Failed, Options", exact: true }).waitFor();
  await page.getByText("The operation timed out", { exact: true }).waitFor();
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  // Retry works beside other drafts and leaves them staged, but not while
  // that setting's own draft differs or is invalid.
  await backlight.click();
  assert.equal(await retryButton.getAttribute("aria-disabled"), "false");
  await hands.fill("45");
  assert.equal(await retryButton.getAttribute("aria-disabled"), "true");
  await hands.fill("47");
  assert.equal(await retryButton.getAttribute("aria-disabled"), "true");
  await hands.fill("60");
  assert.equal(await retryButton.getAttribute("aria-disabled"), "false");
  await retryButton.click();
  assert.deepEqual((await saves()).at(-1).changes, [{ type: "set", setting: "backlight.delay.hands_out", value: 60 }]);
  await page.getByRole("button", { name: "Backlight, Changed, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Undo Change", exact: true }).click();
  // Once the setting shows the saved value applied, the failure and Retry go away.
  Object.assign(handsSaved, { state: "applied", error: null, value: 60 });
  await publish();
  await page.getByRole("button", { name: "Timeout With Hands Away, Saved, Options", exact: true }).waitFor();
  assert.equal(await page.getByText("The operation timed out", { exact: true }).count(), 0);
  assert.equal(await retryButton.count(), 0);
  // A refused save says why for each change it held.
  entry.settingsSave = {
    running: false,
    items: [
      { change: { type: "set", setting: "backlight.level", value: 5 }, status: "not_saved", error: "The adapter is busy" },
      { change: { type: "forget", setting: "backlight.mode" }, status: "not_saved", error: "The adapter is busy" },
    ],
  };
  await publish();
  assert.equal(await page.getByText("Couldn't Save: The adapter is busy", { exact: true }).count(), 2);
  await page.getByText("Couldn't Save 2", { exact: true }).waitFor();
  entry.settingsSave = null;

  // Logitech Features off still saves.
  entry.device.hidpp = { ...entry.device.hidpp, enabled: false, state: "off" };
  await publish();
  await backlight.click();
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  await discard.click();
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  Object.assign(enabled, { saved: true, value: true, state: "applied" });

  // Disconnected, the readings show dim and values still save: the adapter
  // applies them when the device connects. Refresh needs the connection.
  entry.device.hidpp = { ...entry.device.hidpp, enabled: true, state: "disconnected" };
  entry.device.state = "disconnected";
  await publish();
  await refresh.and(page.locator('[aria-disabled="true"]')).waitFor();
  assert.ok(await page.locator(".setting-row.stale").count() > 0);
  assert.equal(await backlight.isDisabled(), false);
  await hands.fill("45");
  await page.getByRole("button", { name: "Backlight, Saved, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Forget Saved Value", exact: true }).click();
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  await save.click();
  assert.deepEqual((await saves()).at(-1).changes, [
    { type: "forget", setting: "backlight.enabled" },
    { type: "set", setting: "backlight.delay.hands_out", value: 45 },
  ]);
  await discard.click();

  // Connecting, the Details bar offers Disconnect, which stops it.
  entry.device.state = "connecting";
  await publish();
  await tab("Details").click();
  await bar.getByRole("button", { name: "Disconnect", exact: true }).click();
  assert.ok((await actions()).some((a) => a.type === "device.disconnect" && a.key === entry.key));
  // A failed connection says why on Diagnostics, never as a banner.
  entry.device.state = "disconnected";
  entry.device.error = "connection_failed";
  await publish();
  await tab("Diagnostics").click();
  await page.getByText("The Bluetooth link or HID setup failed", { exact: true }).waitFor();
  assert.equal(await page.locator(".banner").count(), 0);
  await tab("Details").click();
  assert.equal(await page.getByText("The Bluetooth link or HID setup failed", { exact: true }).count(), 0);
  entry.device.error = null;

  // There is no Settings tab until the device has settings, even while HID++ starts; a failed
  // read is shown on Diagnostics.
  entry.device.state = "connected";
  entry.device.hidpp = { ...entry.device.hidpp, state: "starting" };
  const settings = entry.settings;
  const info = entry.device.info;
  entry.settings = [];
  entry.device.info = [];
  await publish();
  await tab("Diagnostics").waitFor();
  assert.equal(await tab("Settings").count(), 0);
  entry.settingsError = "The adapter is busy";
  await publish();
  await tab("Diagnostics").click();
  await page.getByText("The adapter couldn't read the device's settings. The adapter is busy", { exact: true }).waitFor();
  assert.equal(await tab("Settings").count(), 0);
  // Retry reads the lists again.
  await bar.getByRole("button", { name: "Retry", exact: true }).click();
  assert.equal((await actions()).at(-1).type, "device.reload");
  entry.settingsError = null;
  entry.settings = settings;
  entry.device.info = info;
  entry.device.hidpp = { ...entry.device.hidpp, state: "active" };
  await publish();
  await tab("Settings").click();
  await page.getByRole("group", { name: "Backlight", exact: true }).waitFor();

  // Details keeps the device's own facts; HID++, warnings, security and
  // identifiers are on Diagnostics, which refreshes the connected device.
  await tab("Details").click();
  const fact = (label) => page.locator(".fact").filter({ has: page.locator("dt", { hasText: new RegExp(`^${label}$`) }) }).locator("dd");
  await fact("Manufacturer").waitFor();
  for (const label of ["HID\\+\\+ Protocol", "Device ID", "Vendor ID", "Encrypted"]) assert.equal(await fact(label).count(), 0, label);
  await tab("Diagnostics").click();
  await page.getByText("4.5", { exact: true }).waitFor();
  assert.equal(await fact("Status").textContent(), "Active");
  await fact("Device ID").waitFor();
  assert.equal(await page.getByText("Special-Key Translation", { exact: true }).count(), 0);
  // A device without warnings has no warnings section.
  assert.equal(await page.getByRole("heading", { name: "Device Warnings" }).count(), 0);
  entry.device.hidpp = { ...entry.device.hidpp, state: null, error: "protocol_unsupported" };
  await publish();
  await page.getByText("Failed: Not supported", { exact: true }).waitFor();
  entry.device.hidpp = { ...entry.device.hidpp, state: "active", error: null };
  await publish();
  await fail("device.refresh");
  await bar.getByRole("button", { name: "Refresh", exact: true }).click();
  await bar.getByText("Couldn't save this value.", { exact: true }).waitFor();
  await fail(null);
  await bar.getByRole("button", { name: "Retry", exact: true }).click();
  await bar.getByRole("button", { name: "Refresh", exact: true }).waitFor();

  // HID warnings are listed on Diagnostics, naming where they apply, never as a banner.
  const fieldWarning = "The adapter can't derive a value from this numeric selector field.";
  entry.warnings = [
    { code: "numeric_selector_unsupported", service: 1, reportId: 3, reportType: "input", bitOffset: 16, usagePage: 0x0c, usage: 0x238 },
    { code: "indicator_write_failed", service: 0, reportId: null, reportType: "output", bitOffset: null, usagePage: null, usage: null },
  ];
  await publish();
  await page.getByText(fieldWarning).waitFor();
  const labels = await page.locator(".fact dt").filter({ visible: true }).allTextContents();
  assert.deepEqual(labels.slice(0, 2), ["Input Field", "Lock Indicators"]);
  await page.getByText("Service 1, Input Report 3, Bit 16, Usage 000C:0238", { exact: true }).waitFor();
  await page.getByText("Service 0, Output Report", { exact: true }).waitFor();
  assert.equal(await page.locator(".banner").filter({ hasText: fieldWarning }).count(), 0);
  await tab("Details").click();
  await fact("Manufacturer").waitFor();
  assert.equal(await page.getByText(fieldWarning).count(), 0);
  await tab("Diagnostics").click();
  // A failed read of the list reports in the card and offers Retry.
  entry.warnings = null;
  entry.warningsError = "The adapter is busy";
  await publish();
  await page.getByText("The adapter couldn't read the device's warnings.", { exact: true }).waitFor();
  await bar.getByRole("button", { name: "Retry", exact: true }).click();
  assert.equal((await actions()).at(-1).type, "device.reload");
  entry.warnings = [];
  entry.warningsError = null;
  await publish();
  await bar.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  await tab("Details").click();

  // Turning a device on isn't offered while its transport's places are full.
  const use = page.getByRole("switch", { name: "Use This Device", exact: true });
  assert.equal(await use.isDisabled(), false);
  entry.device.enabled = false;
  entry.device.inactive = "disabled";
  state.adapters.find((a) => a.id === entry.adapterId).status.transports.find((t) => t.transport === "ble").maxEnabled = 1;
  await publish();
  await use.and(page.locator(":disabled")).waitFor();
  state.adapters.find((a) => a.id === entry.adapterId).status.transports.find((t) => t.transport === "ble").maxEnabled = 7;
  await publish();
  await use.and(page.locator(":enabled")).waitFor();
  entry.device.enabled = true;
  entry.device.inactive = null;
  await publish();

  // Details changes stage until Save, which sends them in one request; a failed save keeps them.
  const trust = page.getByRole("switch", { name: "Automatic Connections", exact: true });
  const trusted = await trust.isChecked();
  const sent = (await actions()).length;
  await trust.click();
  await page.getByRole("img", { name: "Automatic Connections, Changed", exact: true }).waitFor();
  assert.equal((await actions()).length, sent);
  await fail("device.update");
  await bar.getByRole("button", { name: "Save", exact: true }).click();
  await bar.getByText("Couldn't save this value.", { exact: true }).waitFor();
  assert.equal(await trust.isChecked(), !trusted);
  await fail(null);
  await page.keyboard.press("Control+s");
  await page.getByRole("img", { name: "Automatic Connections, Changed", exact: true }).waitFor({ state: "detached" });
  assert.deepEqual((await actions()).slice(sent), [
    { type: "device.update", key: entry.key, trusted: !trusted },
    { type: "device.update", key: entry.key, trusted: !trusted },
  ]);

  // A device without settings shows its details without a Settings tab, and
  // the tab chosen earlier returns with it. A focused control keeps its focus.
  await tab("Settings").click();
  entry.settings = [];
  entry.device.info = info.filter((f) => f.key.startsWith("device."));
  entry.device.hidpp = { ...entry.device.hidpp, state: "unsupported" };
  await publish();
  await tab("Settings").waitFor({ state: "detached" });
  const automatic = page.getByRole("switch", { name: "Automatic Connections", exact: true });
  await automatic.focus();
  // HID++ starting again does not bring the tab back; its settings do.
  entry.device.hidpp = { ...entry.device.hidpp, state: "starting" };
  await publish();
  assert.equal(await tab("Settings").count(), 0);
  entry.settings = settings;
  await publish();
  await page.getByRole("tab", { name: "Settings", selected: true }).waitFor();
  await tab("Details").click();
  await automatic.focus();
  entry.settings = [];
  entry.device.hidpp = { ...entry.device.hidpp, state: "unsupported" };
  await publish();
  await tab("Settings").waitFor({ state: "detached" });
  assert.equal(await automatic.evaluate((e) => e === document.activeElement), true);
  entry.settings = settings;
  entry.device.info = info;
  entry.device.hidpp = { ...entry.device.hidpp, state: "active" };
  await publish();
  await page.getByRole("tab", { name: "Details", selected: true }).waitFor();
  assert.equal(await automatic.evaluate((e) => e === document.activeElement), true);

  // The Profiles tab lists the device's layers in order; changes stage until its own Save.
  const owner = state.adapters.find((a) => a.id === entry.adapterId);
  owner.profileNames = { 1: { id: 1, name: "Typing", roles: ["keyboard"] }, 2: { id: 2, name: "Scrolling", roles: ["mouse"] } };
  owner.profilePage = { profiles: Object.values(owner.profileNames), unreadable: [], previous: false, next: false, loading: false, error: null };
  owner.pickerPage = owner.profilePage;
  entry.device.state = "connected";
  entry.device.profiles = [1];
  await publish();
  assert.deepEqual(await page.getByRole("tab").allTextContents(), ["Details", "Settings", "Profiles", "Diagnostics"]);
  await tab("Profiles").click();
  await page.getByRole("button", { name: "Typing, Remove", exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Typing, Move Up", exact: true }).isDisabled(), true);
  await page.getByRole("button", { name: "Add Profile", exact: true }).click();
  const add = page.getByRole("dialog", { name: "Add Profile", exact: true });
  await add.getByRole("radio", { name: "Scrolling", exact: true }).check();
  await add.getByRole("button", { name: "Choose", exact: true }).click();
  await add.waitFor({ state: "hidden" });
  await page.getByRole("button", { name: "Scrolling, Move Up", exact: true }).click();
  await page.getByRole("img", { name: "Profiles, Changed", exact: true }).waitFor();
  // A staged layer change belongs to Profiles; Details has nothing to save.
  await tab("Details").click();
  assert.equal(await save.isDisabled(), true);
  await tab("Profiles").click();
  await save.click();
  assert.deepEqual((await actions()).at(-1), { type: "device.update", key: entry.key, profiles: [2, 1] });
  entry.device.profiles = [2, 1];
  await publish();
  await page.getByRole("img", { name: "Profiles, Changed", exact: true }).waitFor({ state: "detached" });
  // A connected device whose profiles aren't loaded says why on Diagnostics.
  entry.device.profileError = "no_capacity";
  await publish();
  await tab("Diagnostics").click();
  await page.getByText("The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles.", { exact: true }).waitFor();
  await tab("Details").click();
  entry.device.profileError = null;
  await publish();

  state.scan = { adapterId: entry.adapterId, running: false, candidates: [], error: null };
  await publish();
  await page.getByRole("button", { name: "Add Device", exact: true }).click();
  const pairing = page.getByRole("dialog", { name: "Add Device", exact: true });
  await pairing.getByText("No Devices Found", { exact: true }).waitFor();
  await pairing.getByRole("button", { name: "Refresh", exact: true }).click();
  assert.ok((await actions()).some((a) => a.type === "scan.start" && a.adapterId === entry.adapterId));
  state.scan.error = "The search failed.";
  await publish();
  await pairing.getByRole("button", { name: "Retry", exact: true }).waitFor();
  state.scan.candidates = [{ id: 7, name: "Nearby Keyboard", transport: "ble", kinds: ["keyboard"], rssi: null }];
  await publish();
  await pairing.getByRole("button", { name: "Pair", exact: true }).waitFor();
  state.scan.error = null;
  await publish();
  await pairing.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  await fail("pair.start");
  await pairing.getByRole("button", { name: "Pair", exact: true }).click();
  await pairing.getByText("Couldn't save this value.", { exact: true }).waitFor();
  await pairing.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  assert.equal(await pairing.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  await fail(null);
  // A full adapter can't add another device.
  const status = state.adapters.find((a) => a.id === entry.adapterId).status;
  status.info.push({ key: "storage.full", type: "bool", value: true });
  await publish();
  await pairing.getByText("Storage Full", { exact: true }).waitFor();
  assert.equal(await pairing.getByRole("button", { name: "Pair", exact: true }).count(), 0);
  status.info.pop();
  await publish();
  // Each prompt step asks for what it needs.
  state.pairing = { adapterId: entry.adapterId, candidateId: 7, name: "Nearby Keyboard", phase: "pairing", prompt: null, deviceKey: null, message: null };
  await publish();
  await pairing.getByText("Pairing with Nearby Keyboard…", { exact: true }).waitFor();
  state.pairing.prompt = { kind: "show", code: "passkey", value: "042731" };
  await publish();
  await pairing.getByText("0 4 2 7 3 1", { exact: true }).waitFor();
  state.pairing.prompt = { kind: "enter", code: "pin" };
  await publish();
  await pairing.getByRole("textbox", { name: "PIN", exact: true }).fill("0000");
  await pairing.getByRole("button", { name: "Pair", exact: true }).click();
  assert.deepEqual((await actions()).at(-1), { type: "pair.reply", accept: true, value: "0000" });
  state.pairing = { ...state.pairing, phase: "failed", prompt: null, message: "Pairing failed." };
  await publish();
  await pairing.getByRole("button", { name: "Retry", exact: true }).click();
  assert.ok((await actions()).some((a) => a.type === "pair.dismiss"));
  state.pairing.phase = "cancelled";
  state.pairing.message = "Adding the device was cancelled.";
  await publish();
  await pairing.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  assert.equal(await pairing.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  state.pairing.phase = "connected";
  state.pairing.message = null;
  await publish();
  await pairing.getByRole("button", { name: "Add Another", exact: true }).waitFor();
  await pairing.getByRole("button", { name: "Done", exact: true }).click();
  state.pairing = null;
  state.scan = null;
  state.adapters = [];
  state.devices = [];
  await page.getByRole("button", { name: "Overview", exact: true }).click();
  await publish();
  await page.getByRole("button", { name: "Refresh Adapters", exact: true }).click();
  assert.ok((await actions()).some((a) => a.type === "adapters.refresh"));
  assert.deepEqual(errors, []);
  console.log("Device UI: settings form, recovery, and Refresh/Retry labels passed");
} catch (error) {
  console.error(await page.locator("body").innerText());
  throw error;
} finally {
  await app.close();
  await rm(profile, { recursive: true, force: true });
}
