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

/** Runs an action, reporting a failure; resolves with the result. */
export async function act(action: Action): Promise<ActionResult> {
  const result = await api.act(action);
  if (!result.ok) report(result.message);
  return result;
}

/** An action runner that tracks whether its action is in progress. */
export function useAction(): [boolean, (action: Action) => Promise<ActionResult>] {
  const [busy, setBusy] = useState(false);
  const run = useCallback(async (action: Action) => {
    setBusy(true);
    try {
      return await act(action);
    } finally {
      setBusy(false);
    }
  }, []);
  return [busy, run];
}
