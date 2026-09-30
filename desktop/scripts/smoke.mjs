// Launches the built app with simulated adapters, walks through the main
// views and saves screenshots. Run under a display, e.g. xvfb-run.
import { _electron as electron } from "playwright";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const out = process.argv[2] ?? "out/screenshots";
const scheme = process.argv[3] ?? "light";
mkdirSync(out, { recursive: true });
// A separate profile, so a running instance's single-instance lock and
// preferences are untouched. Notifications are off: they would reach the
// desktop the run shares a session bus with.
const profile = mkdtempSync(join(tmpdir(), "cordial-smoke-"));
writeFileSync(join(profile, "preferences.json"), JSON.stringify({ notifyLowBattery: false, notifyConnections: false }));
const app = await electron.launch({
  executablePath: process.env.CORDIAL_EXECUTABLE,
  chromiumSandbox: true,
  args: [...(process.env.CORDIAL_EXECUTABLE ? [] : ["."]), `--user-data-dir=${profile}`, `--force-dark-mode=${scheme === "dark"}`],
  env: { ...process.env, CORDIAL_DESKTOP_SIMULATE: "2", ELECTRON_ENABLE_LOGGING: "1" },
});
const errors = [];
const page = await app.firstWindow();
page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
page.on("pageerror", (e) => errors.push(String(e)));
await page.emulateMedia({ colorScheme: scheme });
// Resize the window itself: the title bar reserves room for the window
// controls from the window's size, which a viewport override leaves alone.
await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setContentSize(1000, 720));
const shot = async (name) => {
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${out}/${scheme}-${name}.png` });
};

await page.getByRole("button", { name: /Example Keys/ }).first().waitFor();
await shot("home");
await page.getByRole("button", { name: /Example Keys/ }).first().click();
await shot("device");
await page.getByRole("tab", { name: "Details" }).click();
await shot("device-details");
await page.getByRole("button", { name: /Example Mouse/ }).first().click();
await shot("mouse");
await page.getByRole("button", { name: /Old Mouse/ }).first().click();
await shot("needs-pairing");
await page.getByRole("button", { name: /Pico W/ }).first().click();
await shot("adapter");
await page.getByRole("button", { name: "Add Device" }).click();
await page.getByText("Example Keys Mini").waitFor();
await shot("add-device");
await page.getByRole("listitem").filter({ hasText: "Example Keys Mini" }).getByRole("button", { name: "Pair" }).click();
await page.getByText("Does Example Keys Mini show this code?").waitFor();
await shot("pair-confirm");
await page.getByRole("button", { name: "Yes, It Matches" }).click();
await page.getByText("is paired and connected").waitFor();
await shot("paired");
await page.getByRole("button", { name: "Done" }).click();
// Native menus are outside the page: keep the app menu instead of showing
// it, then choose its item from the main process.
await app.evaluate(({ Menu }) => {
  Menu.prototype.popup = function () {
    globalThis.smokeMenu = this;
  };
});
await page.getByRole("button", { name: "Main Menu" }).click();
await app.evaluate(async () => {
  while (!globalThis.smokeMenu) await new Promise((r) => setTimeout(r, 50));
  globalThis.smokeMenu.items.find((item) => item.label === "Preferences…").click();
});
await page.getByRole("dialog", { name: "Preferences" }).waitFor();
await shot("preferences");
await page.keyboard.press("Escape");
const state = await page.evaluate(() => window.cordial.state());
if (state.adapters.length !== 2 || !state.devices.some((d) => d.name === "Example Keys Mini")) throw new Error("unexpected state");
await app.close();
if (errors.length) {
  console.error(errors.join("\n"));
  process.exit(1);
}
console.log(`screenshots in ${out}`);
