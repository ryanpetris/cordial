// Web build entry: checks the browser can reach adapters, then starts the
// controller in the page and shows the window. `?simulate=N` runs a demo
// with N simulated adapters instead.
import "./web.css";
import { startWebBackend } from "./backend.ts";

/** Replaces the page with a short explanation. */
function explain(title: string, text: string, link?: { href: string; label: string }) {
  const box = document.createElement("div");
  box.className = "web-message";
  const heading = document.createElement("h1");
  heading.textContent = title;
  const paragraph = document.createElement("p");
  paragraph.textContent = text;
  box.append(heading, paragraph);
  if (link) {
    const a = document.createElement("a");
    a.href = link.href;
    a.textContent = link.label;
    box.append(a);
  }
  document.getElementById("root")!.replaceChildren(box);
}

/** Holds a lock for the page's lifetime; false when another tab has it. */
function exclusive(): Promise<boolean> {
  if (!navigator.locks) return Promise.resolve(true);
  return new Promise((resolve) => {
    void navigator.locks.request("cordial-adapters", { ifAvailable: true }, (lock) => {
      resolve(lock !== null);
      return lock ? new Promise<void>(() => {}) : undefined;
    });
  });
}

const params = new URLSearchParams(location.search);
const simulate = params.has("simulate") ? Math.min(Math.max(Math.trunc(Number(params.get("simulate"))) || 2, 1), 4) : 0;
const demo = { href: "?simulate=2", label: "Try it with simulated adapters" };

if (!simulate && !navigator.serial)
  explain("This browser can't reach adapters", "Cordial needs Web Serial. Open this page in Chrome, Edge or another Chromium-based browser on a computer.", demo);
else if (!simulate && !(await exclusive()))
  explain("Cordial is already open", "Only one tab can manage adapters. Use the other tab, or close it and reload this one.");
else {
  window.cordial = await startWebBackend(simulate);
  await import("../renderer/main.tsx");
}
