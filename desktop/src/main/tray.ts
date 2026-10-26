// The tray icon. Each image change makes GNOME's AppIndicator extension reload
// the icon from a new file, and it can miss a change that arrives during a
// reload and keep showing a stale icon. The image therefore changes only when
// it differs from the one shown, and at most once a second. The tooltip and
// menu change at once.

export const IMAGE_INTERVAL = 1000;

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
  let cooldown: ReturnType<typeof setTimeout> | null = null;

  const cool = () => {
    cooldown = setTimeout(() => {
      cooldown = null;
      apply();
    }, IMAGE_INTERVAL);
  };
  // Shows the latest requested image. A request that returned to the shown
  // image during a cooldown leaves nothing to do.
  const apply = () => {
    if (!tray || wanted === shown) return;
    tray.setImage(wanted);
    shown = wanted;
    cool();
  };

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
      tray?.destroy();
      tray = null;
      key = null;
    },
  };
}
