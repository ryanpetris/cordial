// The tray icon. Each image change makes GNOME's AppIndicator extension reload
// the icon from a new file, and it can miss a change that arrives during a
// reload and keep showing a stale icon. The image therefore changes only when
// it differs from the one shown. Updates apply immediately.

export interface TrayHandle {
  setImage(image: string): void;
  destroy(): void;
}

/** Owns the tray made by `create`, which takes the path of its first image. */
export function trayIcon<T extends TrayHandle>(create: (image: string) => T) {
  let tray: T | null = null;
  let key: string | null = null;
  let shown = "";

  return {
    /** Shows `image`, and calls `describe` to set the tooltip and menu when the tray is new or `next` differs from the last key. */
    update(image: string, next: string, describe: (tray: T) => void) {
      if (!tray) {
        tray = create(image);
        shown = image;
      } else if (image !== shown) {
        tray.setImage(image);
        shown = image;
      }
      if (next !== key) {
        key = next;
        describe(tray);
      }
    },
    /** Destroys the tray. */
    hide() {
      tray?.destroy();
      tray = null;
      key = null;
    },
  };
}
