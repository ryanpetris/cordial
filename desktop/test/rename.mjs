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
  const rename = page.getByRole('button', { name: 'Rename…', exact: true });
  await rename.click();
  const input = page.getByRole('textbox', { name: 'Adapter name' });
  assert.equal(await input.inputValue(), 'Pico W');
  await input.fill('é'.repeat(33));
  assert.equal(await page.getByRole('button', { name: 'Rename', exact: true }).isDisabled(), true);
  await input.fill('Desk \u{10400}');
  await page.getByRole('button', { name: 'Rename', exact: true }).click();
  await input.waitFor({ state: 'hidden' });
  await page.getByRole('heading', { name: 'Desk \u{10400}', exact: true }).waitFor({ state: 'attached' });
  await rename.click();
  assert.equal(await input.inputValue(), 'Desk \u{10400}');
  await input.fill('Cancelled');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await page.getByRole('heading', { name: 'Desk \u{10400}', exact: true }).waitFor({ state: 'attached' });
  await rename.click();
  await page.getByRole('button', { name: 'Reset to default', exact: true }).click();
  await input.waitFor({ state: 'hidden' });
  await page.getByRole('heading', { name: 'Pico W', exact: true }).waitFor({ state: 'attached' });
  // Give both adapters the same name and verify the displayed and editable values.
  await rename.click();
  await input.fill('XIAO ESP32-S3');
  await page.getByRole('button', { name: 'Rename', exact: true }).click();
  await input.waitFor({ state: 'hidden' });
  await page.getByRole('heading', { name: 'XIAO ESP32-S3', exact: true }).waitFor({ state: 'attached' });
  await rename.click();
  assert.equal(await input.inputValue(), 'XIAO ESP32-S3');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await page.getByRole('button', { name: 'Connect', exact: true }).waitFor();
  assert.equal(await rename.isDisabled(), true);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await page.getByRole('button', { name: 'Disconnect', exact: true }).waitFor();
  await page.getByRole('heading', { name: 'XIAO ESP32-S3', exact: true }).waitFor({ state: 'attached' });
  await app.evaluate(({ ipcMain }) => {
    ipcMain.removeHandler('act');
    ipcMain.handle('act', () => ({ ok: false, message: 'Could not save name' }));
  });
  await rename.click();
  await input.fill('Failed draft');
  await page.getByRole('button', { name: 'Rename', exact: true }).click();
  await page.getByText('Could not save name', { exact: true }).waitFor();
  assert.equal(await input.inputValue(), 'Failed draft');
  await app.evaluate(({ ipcMain }) => {
    ipcMain.removeHandler('act');
    ipcMain.handle('act', () => new Promise((resolve) => { globalThis.renameReply = resolve; }));
  });
  await page.getByRole('button', { name: 'Rename', exact: true }).click();
  assert.equal(await page.getByRole('button', { name: 'Cancel', exact: true }).isDisabled(), true);
  await page.keyboard.press('Escape');
  assert.equal(await input.isVisible(), true);
  await app.evaluate(() => { globalThis.renameReply({ ok: true }); });
  await input.waitFor({ state: 'hidden' });
  console.log('Desktop rename UI: save, reset, Unicode limit, cancel, errors, pending state, duplicate names and reconnect passed');
} catch (e) {
  const page = await app.firstWindow();
  console.error(await page.locator('body').innerText());
  throw e;
} finally {
  await app.close();
  await rm(profile, { recursive: true, force: true });
}
