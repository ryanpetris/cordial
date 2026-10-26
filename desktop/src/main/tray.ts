// The tray icon. Each image change makes GNOME's AppIndicator extension reload
// the icon from a new file, and it can miss a change that arrives during a
// reload and keep showing a stale icon. The image therefore changes only when
// it differs from the one shown, and at most once a second. The tooltip and
// menu change at once.
//
// Startup races in the shell can also leave a stale icon. As a bounded attempt
// to recover, a shown tray re-sends its image at fixed times in the first
// minute after the controller is made, once per launch. This makes a correct
// icon more likely but cannot guarantee the shell draws it.

export const IMAGE_INTERVAL = 1000;

/** Milliseconds after launch at which a shown tray re-sends its image. */
const REFRESHES = [1, 2, 5, 10, 15, 30, 60].map((s) => s * 1000);

export interface TrayHandle {
  setImage(image: string): void;
  destroy(): void;
}

/** Owns the tray made by `create`, which takes the path of its first image. */
export function trayIcon<T extends TrayHandle>(create: (image: string) => T) {
  let tray: T | null = null;
  let key: string | null = null;
  let shown = "";
  let wanted = "";
  let force = false;
  let cooldown: ReturnType<typeof setTimeout> | null = null;

  const cool = () => {
    cooldown = setTimeout(() => {
      cooldown = null;
      apply();
    }, IMAGE_INTERVAL);
  };
  // Shows the latest requested image. A request that returned to the shown
  // image during a cooldown leaves nothing to do unless a refresh is due.
  const apply = () => {
    if (!tray || (wanted === shown && !force)) return;
    tray.setImage(wanted);
    shown = wanted;
    force = false;
    cool();
  };
  // A hidden tray skips the refresh rather than keeping it for later.
  for (const at of REFRESHES) {
    setTimeout(() => {
      if (!tray) return;
      force = true;
      if (!cooldown) apply();
    }, at);
  }

  return {
    /** Shows `image`, and calls `describe` to set the tooltip and menu when the tray is new or `next` differs from the last key. */
    update(image: string, next: string, describe: (tray: T) => void) {
      wanted = image;
      if (!tray) {
        tray = create(image);
        shown = image;
        cool();
      } else if (!cooldown) apply();
      if (next !== key) {
        key = next;
        describe(tray);
      }
    },
    /** Destroys the tray along with any image change still waiting. */
    hide() {
      if (cooldown) clearTimeout(cooldown);
      cooldown = null;
      force = false;
      tray?.destroy();
      tray = null;
      key = null;
    },
  };
}
