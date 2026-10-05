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
await shot("device-details");
const deviceTabs = await page.getByRole("tab").allTextContents();
if (deviceTabs.join() !== "Details,Settings,Profiles,Diagnostics") throw new Error(`unexpected device tabs: ${deviceTabs}`);
await page.getByRole("tab", { name: "Profiles" }).click();
await page.getByRole("button", { name: "Typing, Remove", exact: true }).waitFor();
await shot("device-profiles");
await page.getByRole("tab", { name: "Diagnostics" }).click();
await shot("device-diagnostics");
await page.getByRole("tab", { name: "Settings" }).click();
await shot("device");
await page.getByRole("button", { name: /Example Mouse/ }).first().click();
await shot("mouse");
await page.getByRole("tab", { name: "Diagnostics" }).click();
await shot("mouse-diagnostics");
await page.getByRole("button", { name: /Old Mouse/ }).first().click();
await shot("disabled");
await page.getByRole("button", { name: /Pico 2 W/ }).first().click();
const tabs = await page.getByRole("tab").allTextContents();
if (tabs.join() !== "Details,Settings,Profiles,Diagnostics") throw new Error(`unexpected adapter tabs: ${tabs}`);
await shot("adapter");
await page.getByRole("tab", { name: "Settings" }).click();
await page.getByRole("radiogroup", { name: "Platform" }).waitFor();
await shot("adapter-settings");
await page.getByRole("tab", { name: "Diagnostics" }).click();
await page.getByText("Adapter ID").waitFor();
await shot("adapter-diagnostics");
await page.getByRole("tab", { name: "Profiles" }).click();
const smokeAdapter = (await page.evaluate(() => window.cordial.state())).adapters.find((a) => a.name.startsWith("Pico 2 W") && a.status?.profileSupport);
const viaEnabled = () => page.evaluate((id) => window.cordial.state().then((s) => s.adapters.find((a) => a.id === id)?.status?.interfaces.find((i) => i.interface === 1)?.enabled), smokeAdapter.id);
const save = page.locator(".page-bar").getByRole("button", { name: "Save", exact: true });
// Interface changes stage until Save; one that reconnects USB asks first.
const via = page.getByRole("switch", { name: "VIA", exact: true });
const before = await viaEnabled();
if (!before) {
  await page.getByRole("button", { name: "VIA Profile", exact: true }).click();
  const picker = page.getByRole("dialog", { name: "VIA Profile" });
  await picker.getByRole("radio").nth(1).check();
  await picker.getByRole("button", { name: "Choose", exact: true }).click();
  await picker.waitFor({ state: "hidden" });
}
await via.click();
const confirm = page.getByRole("dialog", { name: "USB Reconnect Required" });
await save.click();
await confirm.getByText("The adapter will disconnect from this computer for a moment after it saves these changes.").waitFor();
await shot("adapter-interface");
await confirm.getByRole("button", { name: "Cancel" }).click();
await confirm.waitFor({ state: "hidden" });
if ((await viaEnabled()) !== before) throw new Error("the interface changed without confirmation");
await save.click();
await confirm.getByRole("button", { name: "Save" }).click();
await confirm.waitFor({ state: "hidden" });
// The adapter reconnects USB; its page and chosen tab stay through it.
const profilesTab = page.getByRole("tab", { name: "Profiles" });
await page.waitForFunction(async ([id, v]) => {
  const a = (await window.cordial.state()).adapters.find((x) => x.id === id);
  return a?.connection === "connected" && a.readiness === "ready" && a.profilePage && !a.profilePage.loading
    && a.status?.interfaces.find((i) => i.interface === 1)?.enabled === v;
}, [smokeAdapter.id, !before]);
await page.waitForFunction(() => document.querySelector(".page-bar button.suggested")?.disabled);
if ((await profilesTab.getAttribute("aria-selected")) !== "true") throw new Error("the adapter tab changed across a reconnect");
// Creating and copying profiles act at once.
await page.getByRole("button", { name: "New Profile" }).click();
const created = page.getByRole("dialog", { name: "New Profile" });
await created.getByRole("textbox", { name: "Profile name" }).fill("Travel");
await created.getByRole("button", { name: "Create" }).click();
await created.waitFor({ state: "hidden" });
await page.getByRole("button", { name: "Travel, Options", exact: true }).click();
await page.getByRole("menuitem", { name: "Copy" }).click();
const copy = page.getByRole("dialog", { name: "Copy “Travel”" });
await copy.getByRole("button", { name: "Copy" }).click();
await copy.waitFor({ state: "hidden" });
await page.getByRole("button", { name: "Travel Copy, Options", exact: true }).waitFor();
await shot("adapter-profiles");
await page.getByRole("tab", { name: "Details" }).click();
await page.getByRole("button", { name: "Add Device" }).click();
await page.getByText("Example Keys Mini").waitFor();
await shot("add-device");
await page.getByRole("listitem").filter({ hasText: "Example Keys Mini" }).getByRole("button", { name: "Pair" }).click();
await page.getByText("Does Example Keys Mini show this code?").waitFor();
await shot("pair-confirm");
await page.getByRole("button", { name: "Codes Match" }).click();
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
const menuLabels = await app.evaluate(async () => {
  while (!globalThis.smokeMenu) await new Promise((r) => setTimeout(r, 50));
  globalThis.smokeMenu.items.find((item) => item.label === "Settings").click();
  return globalThis.smokeMenu.items.map((item) => item.label).filter(Boolean);
});
if (menuLabels.join() !== "Refresh Adapters,Settings,Quit") throw new Error(`unexpected app menu: ${menuLabels}`);
await page.getByRole("dialog", { name: "Settings" }).waitFor();
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
