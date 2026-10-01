import { Notification } from "electron";

const active = new Set<Notification>();
// Windows timeout leaves notifications clickable in Action Center. The bound
// also covers platforms that don't report every final dismissal.
const MAX_NOTIFICATIONS = 64;

/** Shows a native notification and retains its event callbacks. */
export function showNotification(options: Electron.NotificationConstructorOptions, clicked?: () => void) {
  if (!Notification.isSupported()) return;
  if (active.size >= MAX_NOTIFICATIONS) {
    const oldest = active.values().next().value!;
    active.delete(oldest);
    oldest.close();
  }
  const notification = new Notification(options);
  active.add(notification);
  notification.once("click", () => {
    active.delete(notification);
    clicked?.();
  });
  notification.on("close", (event) => {
    if (event.reason !== "timedOut") active.delete(notification);
  });
  notification.once("failed", () => active.delete(notification));
  notification.show();
}

/** Closes every notification still owned by the app. */
export function closeNotifications() {
  for (const notification of active) notification.close();
  active.clear();
}
