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
  await page.getByRole("group", { name: "Backlight", exact: true }).waitFor();
  const state = await page.evaluate(() => window.cordial.state());
  const entry = state.devices.find((d) => d.name === "Example Keys Wireless");
  assert.ok(entry?.settings?.current);
  const cachedSettings = entry.settings;
  const enabled = entry.settings.settings.find((s) => s.key === "backlight.enabled");
  Object.assign(enabled, { managed: false, desired: null, observed: null, state: "unmanaged" });
  entry.settings.settings.push({ ...enabled, key: "wheel.threshold", type: "integer", feature: 8464, min: 1, max: 255, step: 1, observed: 30 });
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
  await publish();

  const save = page.getByRole("button", { name: "Save", exact: true });
  const discard = page.getByRole("button", { name: "Discard", exact: true });
  const refresh = page.getByRole("button", { name: "Refresh", exact: true });
  const saves = async () => (await actions()).filter((a) => a.type === "settings.save");

  // A failed Refresh reports beside the footer and keeps its name.
  await app.evaluate(() => { globalThis.uiFail = "settings.refresh"; });
  await refresh.click();
  await page.getByText("Couldn't save this value.", { exact: true }).waitFor();
  assert.ok((await actions()).some((a) => a.type === "settings.refresh" && a.key === entry.key));
  assert.equal(await page.locator(".toast").count(), 0);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  await app.evaluate(() => { globalThis.uiFail = null; });

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

  // Values equal to what the device keeps are no change: a typed 030 against
  // a saved 30, and choosing the reading of an unsaved setting again.
  const hands = page.getByRole("spinbutton", { name: "Timeout With Hands Away" });
  await hands.fill("030");
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  assert.equal(await discard.getAttribute("aria-disabled"), "true");
  // It leaves no draft behind to show once the saved value changes.
  const handsSaved = entry.settings.settings.find((s) => s.key === "backlight.delay.hands_out");
  handsSaved.desired = 45;
  await publish();
  assert.equal(await hands.inputValue(), "45");
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  handsSaved.desired = 30;
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
  assert.equal(await threshold.evaluate((e) => e === document.activeElement), true);
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  // A typed 255 is out of the threshold's range, not Off: it stays On and invalid.
  await threshold.fill("255");
  assert.equal(await smart.isChecked(), true);
  assert.equal(await threshold.evaluate((e) => e === document.activeElement), true);
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

  // Drafts survive tabs and other pages.
  await page.getByRole("tab", { name: "Details" }).click();
  await page.getByRole("tab", { name: "Settings" }).click();
  await page.getByRole("button", { name: /Example Mouse/ }).first().click();
  await page.getByRole("button", { name: /Example Keys/ }).first().click();
  assert.equal(await backlight.isChecked(), false);

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
  await app.evaluate(() => { globalThis.uiFail = "settings.save"; });
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
  assert.equal(await page.evaluate(() => document.activeElement?.textContent), "Save");
  assert.equal(await refresh.getAttribute("aria-disabled"), "true");
  assert.equal(await discard.getAttribute("aria-disabled"), "true");
  assert.equal(await backlight.isDisabled(), true);
  await page.getByRole("img", { name: "Timeout With Hands Away, Sending", exact: true }).waitFor();
  assert.equal(await page.locator("form[aria-busy=true]").count(), 1);
  // Logitech Features waits for the settings work too.
  await page.getByRole("tab", { name: "Details" }).click();
  assert.equal(await page.getByRole("switch", { name: "Logitech Features" }).isDisabled(), true);
  await page.getByRole("tab", { name: "Settings" }).click();

  // Saved but not applied: the draft ends, the outcome shows, and Retry resends that value alone.
  const outcome = { running: false, items: [{ change: running.items[0].change, status: "not_applied", error: "No response" }] };
  Object.assign(entry.settings.settings.find((s) => s.key === "backlight.delay.hands_out"), { desired: 60, state: "error", error: "hidpp_timeout" });
  entry.settingsSave = outcome;
  await publish();
  await app.evaluate((_electron, outcome) => { globalThis.uiReply({ ok: false, message: "Some settings weren't applied.", inline: true, settingsSave: outcome }); globalThis.uiHold = null; }, outcome);
  await page.getByText("Didn't Apply: No response", { exact: true }).waitFor();
  await page.getByText("Didn't Apply 1", { exact: true }).waitFor();
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  await page.getByRole("button", { name: "Timeout With Hands Away, Failed, Options", exact: true }).waitFor();
  // Retry works beside other drafts and leaves them staged, but not while
  // that setting's own draft differs or is invalid.
  const retryButton = page.getByRole("button", { name: "Retry", exact: true });
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
  await page.getByRole("button", { name: "Backlight, Changed, Options", exact: true }).waitFor();
  await page.getByRole("button", { name: "Backlight, Changed, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Undo Change", exact: true }).click();
  // A value the device can't take now isn't offered again.
  const handsRow = entry.settings.settings.find((s) => s.key === "backlight.delay.hands_out");
  Object.assign(handsRow, { state: "unsupported", error: null });
  await publish();
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).getAttribute("aria-disabled"), "true");
  // Once the catalog shows the saved value applied, the failure and Retry go away.
  Object.assign(handsRow, { state: "applied", observed: 60 });
  await publish();
  assert.equal(await page.getByText("Didn't Apply 1", { exact: true }).count(), 0);
  assert.equal(await page.getByText("Didn't Apply: No response", { exact: true }).count(), 0);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  // A change held back says why.
  entry.settingsSave = { running: false, items: [{ change: { type: "set", setting: "backlight.level", value: 5 }, status: "not_sent", error: "Backlight Mode isn't permanent manual" }] };
  await publish();
  await page.getByText("Not Sent: Backlight Mode isn't permanent manual", { exact: true }).waitFor();
  await page.getByText("Not Sent 1", { exact: true }).waitFor();
  entry.settingsSave = null;

  // Logitech Features off still saves.
  entry.device.hidpp_enabled = false;
  entry.device.normalization_state = "off";
  await publish();
  await backlight.click();
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  assert.equal(await refresh.getAttribute("aria-disabled"), "false");
  assert.equal(await page.getByRole("button", { name: "Apply Saved Settings", exact: true }).count(), 0);
  await discard.click();
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  Object.assign(enabled, { managed: true, desired: true, observed: true, state: "applied" });

  // Offline, values can't change but forgetting a saved value can be staged
  // and saved; a value change staged earlier stays staged and unsent.
  await hands.fill("45");
  entry.device.state = "disconnected";
  entry.device.normalization_state = "pending";
  entry.settings.current = false;
  await publish();
  assert.equal(await page.getByRole("tab", { name: "Settings" }).isDisabled(), false);
  assert.equal(await backlight.isDisabled(), true);
  assert.equal(await save.getAttribute("aria-disabled"), "true");
  await page.getByRole("button", { name: "Backlight, Saved, Options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Forget Saved Value", exact: true }).click();
  assert.equal(await save.getAttribute("aria-disabled"), "false");
  await save.click();
  assert.deepEqual((await saves()).at(-1).changes, [{ type: "forget", setting: "backlight.enabled" }]);
  assert.equal(await hands.inputValue(), "45");
  await page.getByRole("button", { name: "Timeout With Hands Away, Changed, Options", exact: true }).waitFor();
  entry.device.state = "connecting";
  entry.pending = [{ id: 100, command: "device.connect" }];
  await publish();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  assert.ok((await actions()).some((a) => a.type === "device.connect.cancel" && a.key === entry.key));

  await page.clock.install({ time: new Date("2030-01-01T00:00:00Z") });
  await page.clock.pauseAt(new Date("2030-01-01T00:00:01Z"));
  entry.pending = [];
  entry.device.state = "connected";
  entry.device.settings_state = "pending";
  entry.settings = { settings: [], current: false, state: "pending", error: null, loadError: "The adapter hasn't read these settings yet.", result: null };
  await publish();
  await page.getByText("Reading the device's settings…", { exact: true }).waitFor();
  assert.equal(await page.getByText("Couldn't read the device's settings.", { exact: true }).count(), 0);
  await page.clock.runFor(9999);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  await page.clock.runFor(1);
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  assert.ok((await actions()).some((a) => a.type === "settings.reload"));
  assert.equal(await page.locator(".toast").count(), 0);
  await page.getByText("Reading the device's settings…", { exact: true }).waitFor();
  entry.device.settings_state = "discovering";
  await publish();
  await page.clock.runFor(10_000);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  entry.device.settings_state = "ready";
  await publish();
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  await page.clock.runFor(10_000);
  await page.getByRole("button", { name: "Retry", exact: true }).waitFor();
  entry.device.settings_state = "discovering";
  await publish();
  await page.clock.runFor(90_000);
  await page.getByRole("button", { name: "Retry", exact: true }).waitFor();
  entry.device.settings_state = "ready";
  entry.settings = { ...cachedSettings, current: false, loadError: "Cached list unavailable", result: {
    kind: "apply", error: "Some saved values couldn't be applied.", counts: {
      device_id: entry.device.device_id, revision: 1, count: 6, applied: 5, failed: 1, read: 0, unchanged: 0, unsupported: 0, uncertain: 0,
    },
  } };
  await publish();
  await page.clock.runFor(10_000);
  await page.getByText("Some saved values couldn't be applied.", { exact: true }).waitFor();
  await page.getByText("Couldn't read the device's settings.", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Retry", exact: true }).waitFor();
  entry.settings.result = { kind: "refresh", error: "Some settings couldn't be read.", counts: { ...entry.settings.result.counts, applied: 0, read: 5 } };
  await publish();
  await page.getByRole("button", { name: /Example Mouse/ }).first().click();
  await page.getByRole("button", { name: /Example Keys/ }).first().click();
  await page.clock.runFor(10_000);
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).count(), 1);
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  assert.equal((await actions()).at(-1).type, "settings.reload");
  entry.settings.current = true;
  entry.settings.loadError = null;
  await publish();
  await page.getByText("Some settings couldn't be read.", { exact: true }).waitFor();
  assert.equal(await refresh.getAttribute("aria-disabled"), "false");
  assert.equal(await page.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  entry.settings.current = false;
  entry.settings.loadError = "Cached list unavailable";
  await publish();
  await page.clock.runFor(10_000);
  entry.settings.result = { kind: "refresh", error: null, counts: { ...entry.settings.result.counts, read: 6, applied: 0, failed: 0 } };
  await publish();
  assert.equal(await page.getByText("6 read", { exact: true }).count(), 0);
  await page.getByText("Couldn't read the device's settings.", { exact: true }).waitFor();
  entry.settings.result = null;
  entry.device.settings_error = "settings_unavailable";
  await publish();
  await page.getByRole("button", { name: "Retry", exact: true }).waitFor();
  assert.equal(await page.locator(".toast").count(), 0);

  entry.info = null;
  entry.infoCurrent = false;
  entry.infoError = "device.info got no response";
  await publish();
  await page.getByRole("tab", { name: "Details" }).click();
  await page.getByText("Reading Information…", { exact: true }).waitFor();
  assert.equal(await page.getByText("Couldn't Read Information", { exact: true }).count(), 0);
  await page.clock.runFor(10_000);
  await page.getByText("Couldn't Read Information", { exact: true }).waitFor();
  assert.equal(await page.getByText("device.info got no response", { exact: true }).count(), 0);
  await app.evaluate(() => { globalThis.uiFail = "device.info.refresh"; });
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await page.getByText("Couldn't save this value.", { exact: true }).waitFor();
  assert.equal(await page.locator(".toast").count(), 0);
  await page.getByRole("button", { name: "Retry", exact: true }).waitFor();
  await app.evaluate(() => { globalThis.uiFail = null; });
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  entry.info = [];
  entry.infoCurrent = true;
  entry.infoError = null;
  await publish();
  await page.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  assert.ok((await actions()).some((a) => a.type === "device.info.refresh" && a.key === entry.key));
  await app.evaluate(() => { globalThis.uiFail = "device.info.refresh"; });
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await page.getByRole("button", { name: "Retry", exact: true }).waitFor();
  await page.getByRole("button", { name: "Forget Device…", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("button", { name: "Retry", exact: true }).waitFor();
  await app.evaluate(() => { globalThis.uiFail = null; });
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await page.getByRole("button", { name: "Refresh", exact: true }).waitFor();

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
  state.scan.candidates = [{ candidate_id: "nearby", name: "Nearby Keyboard", transport: "ble", kind: "keyboard", rssi: null }];
  await publish();
  await pairing.getByRole("button", { name: "Pair", exact: true }).waitFor();
  await pairing.getByRole("button", { name: "Retry", exact: true }).waitFor();
  state.scan.error = null;
  await publish();
  await pairing.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  await app.evaluate(() => { globalThis.uiFail = "pair.start"; });
  await pairing.getByRole("button", { name: "Pair", exact: true }).click();
  await pairing.getByText("Couldn't save this value.", { exact: true }).waitFor();
  await pairing.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  assert.equal(await pairing.getByRole("button", { name: "Retry", exact: true }).count(), 0);
  await app.evaluate(() => { globalThis.uiFail = "scan.start"; });
  await pairing.getByRole("button", { name: "Refresh", exact: true }).click();
  await pairing.getByRole("button", { name: "Retry", exact: true }).waitFor();
  await app.evaluate(() => { globalThis.uiFail = null; });
  await pairing.getByRole("button", { name: "Retry", exact: true }).click();
  await pairing.getByRole("button", { name: "Refresh", exact: true }).waitFor();
  state.pairing = {
    adapterId: entry.adapterId, candidateId: "nearby", name: "Nearby Keyboard", phase: "failed", prompt: null, deviceKey: null, message: "Pairing failed.",
  };
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
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
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
