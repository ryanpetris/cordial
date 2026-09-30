// The main process API and small hooks around it.
import { useCallback, useEffect, useState } from "react";
import type { Action, ActionResult, AppState, DesktopApi } from "../shared/state.ts";

declare global {
  interface Window {
    cordial: DesktopApi;
  }
}

export const api = window.cordial;

export function useAppState(): AppState | null {
  const [state, setState] = useState<AppState | null>(null);
  useEffect(() => {
    const off = api.onState(setState);
    void api.state().then((s) => setState((current) => current ?? s));
    return off;
  }, []);
  return state;
}

type Report = (message: string) => void;
let report: Report = () => {};
export const setReporter = (r: Report) => (report = r);
export const reportError = (message: string) => report(message);

/** Opens the adapter chooser; dismissing it is a successful no-op. */
export async function chooseAdapter(): Promise<ActionResult> {
  try {
    await api.host.choosePort?.();
    return { ok: true };
  } catch (error) {
    return error instanceof DOMException && error.name === "NotFoundError"
      ? { ok: true } : { ok: false, message: (error as Error).message };
  }
}

/** Runs an action, reporting a failure unless it is quiet or shown inline; resolves with the result. */
export async function act(action: Action, quiet = false): Promise<ActionResult> {
  let result: ActionResult;
  try { result = await api.act(action); }
  catch { result = { ok: false, message: "Couldn't confirm that action." }; }
  if (!result.ok && !result.inline && !quiet) report(result.message);
  return result;
}

/** An action runner that tracks whether its action is in progress. A quiet
 * runner leaves failures to its caller. */
export function useAction(quiet = false): [boolean, (action: Action) => Promise<ActionResult>] {
  const [busy, setBusy] = useState(false);
  const run = useCallback(
    async (action: Action) => {
      setBusy(true);
      try {
        return await act(action, quiet);
      } finally {
        setBusy(false);
      }
    },
    [quiet],
  );
  return [busy, run];
}
