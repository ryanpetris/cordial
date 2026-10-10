// The main process API and small hooks around it.
import { useCallback, useEffect, useState } from "react";
import type { Action, ActionResult, AppState, DesktopApi } from "../shared/state.ts";
import { asSentence } from "../shared/text.ts";

declare global {
  interface Window {
    cordial: DesktopApi;
  }
}

export const api = window.cordial;

// The latest published state's revision, and actions waiting for a later one.
let revision = 0;
const waiting = new Set<{ revision: number; resolve: () => void }>();

function received(state: AppState) {
  // A lower revision comes from a restarted controller, which no longer has the awaited one.
  const restarted = state.revision < revision;
  revision = state.revision;
  for (const w of waiting) {
    if (restarted || w.revision <= revision) {
      waiting.delete(w);
      w.resolve();
    }
  }
}

api.onState(received);

/** Resolves once the window has received the state with `target` revision or a later one. */
function published(target: number): Promise<void> {
  if (target <= revision) return Promise.resolve();
  return new Promise((resolve) => waiting.add({ revision: target, resolve }));
}

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
      ? { ok: true } : { ok: false, message: asSentence((error as Error).message) };
  }
}

/** Runs an action, reporting a failure unless it is quiet or shown inline; resolves with the
 * result once the window has the state that includes the action's effect, so a caller that
 * replaces staged values with published ones never shows the values from before. */
export async function act(action: Action, quiet = false): Promise<ActionResult> {
  let result: ActionResult;
  try { result = await api.act(action); }
  catch { result = { ok: false, message: "Cordial couldn't confirm whether the action finished. Check the result before trying again." }; }
  if (result.revision !== undefined) await published(result.revision);
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
