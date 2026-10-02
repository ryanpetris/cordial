// Exercises the built desktop app with simulated adapters. Run from desktop:
// xvfb-run -a node test/rename.mjs
import { createRequire } from 'node:module';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
const require = createRequire(path.join(process.cwd(), 'package.json'));
const { _electron } = require('playwright');
const profile = await mkdtemp(path.join(tmpdir(), 'cordial-rename-ui-'));
const env = { ...process.env, CORDIAL_DESKTOP_SIMULATE: '2' };
delete env.ELECTRON_RUN_AS_NODE;
const app = await _electron.launch({ args: ['--no-sandbox', '.', `--user-data-dir=${profile}`], env });
try {
  const page = await app.firstWindow();
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setSize(1100, 900));
  await page.getByRole('button', { name: /^Pico W/ }).first().waitFor();
  await page.getByRole('button', { name: /^Pico W/ }).first().click();
  // Each supported transport has a switch: Classic starts disabled and BLE enabled, as on the
  // adapter. A saved device of a disabled transport says so.
  const classic = page.getByRole('switch', { name: /Bluetooth Classic/ });
  const ble = page.getByRole('switch', { name: /Bluetooth LE/ });
  const checked = async (control, on) => {
    for (let i = 0; i < 100 && (await control.isChecked()) !== on; i++) await page.waitForTimeout(20);
    assert.equal(await control.isChecked(), on);
  };
  const disabledText = 'Bluetooth Classic is disabled. Enable it in the adapter settings.';
  await checked(classic, false);
  await checked(ble, true);
  await page.getByRole('button', { name: /Travel Keyboard/ }).first().click();
  await page.getByText(disabledText, { exact: true }).waitFor();
  await page.getByRole('button', { name: /^Pico W/ }).first().click();
  await classic.click();
  await checked(classic, true);
  await page.getByRole('button', { name: /Travel Keyboard/ }).first().click();
  await page.getByRole('tab', { name: 'Details', exact: true }).waitFor();
  assert.equal(await page.getByText(disabledText, { exact: true }).count(), 0);
  await page.getByRole('button', { name: /^Pico W/ }).first().click();
  await classic.click();
  await checked(classic, false);
  // The simulated ESP32-S3 supports only BLE.
  await page.getByRole('button', { name: /^XIAO ESP32-S3/ }).first().click();
  await checked(ble, true);
  assert.equal(await classic.count(), 0);
  await page.getByRole('button', { name: /^Pico W/ }).first().click();
  // The page bar's Rename opens the dialog; the dialog's Rename submits it.
  const bar = page.locator('.page-bar');
  const rename = bar.getByRole('button', { name: 'Rename', exact: true });
  const submit = page.getByRole('dialog').getByRole('button', { name: 'Rename', exact: true });
  await rename.click();
  const input = page.getByRole('textbox', { name: 'Adapter name' });
  assert.equal(await input.inputValue(), 'Pico W');
  await input.fill('é'.repeat(33));
  assert.equal(await submit.isDisabled(), true);
  await input.fill('Desk \u{10400}');
  await submit.click();
  await input.waitFor({ state: 'hidden' });
  await page.getByRole('heading', { name: 'Desk \u{10400}', exact: true }).waitFor({ state: 'attached' });
  await rename.click();
  assert.equal(await input.inputValue(), 'Desk \u{10400}');
  await input.fill('Cancelled');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await page.getByRole('heading', { name: 'Desk \u{10400}', exact: true }).waitFor({ state: 'attached' });
  await rename.click();
  await page.getByRole('button', { name: 'Reset to Default', exact: true }).click();
  await input.waitFor({ state: 'hidden' });
  await page.getByRole('heading', { name: 'Pico W', exact: true }).waitFor({ state: 'attached' });
  // Give both adapters the same name and verify the displayed and editable values.
  await rename.click();
  await input.fill('XIAO ESP32-S3');
  await submit.click();
  await input.waitFor({ state: 'hidden' });
  await page.getByRole('heading', { name: 'XIAO ESP32-S3', exact: true }).waitFor({ state: 'attached' });
  await rename.click();
  assert.equal(await input.inputValue(), 'XIAO ESP32-S3');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  assert.equal(await page.locator('.page-header button').count(), 0);
  await bar.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await bar.getByRole('button', { name: 'Connect', exact: true }).waitFor();
  assert.equal(await rename.isDisabled(), true);
  await bar.getByRole('button', { name: 'Connect', exact: true }).click();
  await bar.getByRole('button', { name: 'Disconnect', exact: true }).waitFor();
  await page.getByRole('heading', { name: 'XIAO ESP32-S3', exact: true }).waitFor({ state: 'attached' });
  await app.evaluate(({ ipcMain }) => {
    ipcMain.removeHandler('act');
    ipcMain.handle('act', () => ({ ok: false, message: 'Could not save name' }));
  });
  await rename.click();
  await input.fill('Failed draft');
  await submit.click();
  await page.getByText('Could not save name', { exact: true }).waitFor();
  assert.equal(await input.inputValue(), 'Failed draft');
  await app.evaluate(({ ipcMain }) => {
    ipcMain.removeHandler('act');
    ipcMain.handle('act', () => new Promise((resolve) => { globalThis.renameReply = resolve; }));
  });
  await submit.click();
  assert.equal(await page.getByRole('button', { name: 'Cancel', exact: true }).isDisabled(), true);
  await page.keyboard.press('Escape');
  assert.equal(await input.isVisible(), true);
  await app.evaluate(() => { globalThis.renameReply({ ok: true }); });
  await input.waitFor({ state: 'hidden' });
  console.log('Desktop adapter UI: transport switches, rename save, reset, Unicode limit, cancel, errors, pending state, duplicate names and reconnect passed');
} catch (e) {
  const page = await app.firstWindow();
  console.error(await page.locator('body').innerText());
  throw e;
} finally {
  await app.close();
  await rm(profile, { recursive: true, force: true });
}
