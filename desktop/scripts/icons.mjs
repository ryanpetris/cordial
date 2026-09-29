// Draws the tray and application icons as SVG and rasterizes them with
// rsvg-convert. The PNGs are checked in; rerun after changing a design.
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";

const dir = new URL("../assets/icons/", import.meta.url);
mkdirSync(dir, { recursive: true });

// The mark on a 64-unit grid: a C whose upper end sends out a signal.
const C = "M40.5 23.5A15 15 0 1 0 40.5 44.5";
const SIGNAL = "M41.9 15.6A8 8 0 0 1 48.4 22.1M43 9.2A14.5 14.5 0 0 1 54.8 21";
const mark = (color, width, signal) =>
  `<g fill="none" stroke="${color}" stroke-width="${width}" stroke-linecap="round" stroke-linejoin="round"><path d="${signal ? `${C}${SIGNAL}` : C}"/></g><circle cx="40.5" cy="23.5" r="3.4" fill="${color}"/>`;
const COLORS = { light: "#ffffff", dark: "#2e3436" };
const BADGES = {
  low: `<circle cx="17.3" cy="17.3" r="4.4" fill="#e01b24"/><rect x="14.9" y="16.1" width="4.3" height="2.4" rx="0.5" fill="none" stroke="#fff" stroke-width="0.9"/><rect x="19.3" y="16.8" width="0.8" height="1" fill="#fff"/><rect x="15.5" y="16.7" width="1.1" height="1.2" fill="#fff"/>`,
  attention: `<circle cx="17.3" cy="17.3" r="4.4" fill="#e5a50a"/><rect x="16.6" y="14.6" width="1.4" height="3.6" rx="0.7" fill="#241f31"/><circle cx="17.3" cy="19.6" r="0.8" fill="#241f31"/>`,
};

/** The mark alone, with its signal while an adapter is connected. */
function tray(base, badge, color) {
  const cut = badge === "none" ? "" : `<circle cx="17.3" cy="17.3" r="5.9" fill="#000"/>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 22 22">
<mask id="m"><rect width="22" height="22" fill="#fff"/>${cut}</mask>
<g mask="url(#m)"><g transform="translate(-4.45 -1.88) scale(0.444)">${mark(color, 5, base === "connected")}</g></g>${BADGES[badge] ?? ""}</svg>`;
}

const app = `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 64 64">
<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#62a0ea"/><stop offset="1" stop-color="#1c71d8"/></linearGradient></defs>
<rect x="3" y="3" width="58" height="58" rx="14" fill="url(#g)"/>
<g transform="translate(-2.4 2.4)">${mark("#fff", 4.5, true)}</g></svg>`;

const render = (name, svg, sizes) => {
  const svgPath = new URL(`${name}.svg`, dir);
  writeFileSync(svgPath, svg);
  for (const [size, suffix] of sizes)
    execFileSync("rsvg-convert", ["-w", String(size), "-h", String(size), "-o", new URL(`${name}${suffix}.png`, dir).pathname, svgPath.pathname]);
};

for (const base of ["idle", "connected"])
  for (const badge of ["none", "low", "attention"])
    for (const [variant, color] of Object.entries(COLORS))
      render(`tray-${base}-${badge}-${variant}`, tray(base, badge, color), [
        [22, ""],
        [44, "@2x"],
      ]);
render("app", app, [
  [256, ""],
  [512, "-512"],
]);
