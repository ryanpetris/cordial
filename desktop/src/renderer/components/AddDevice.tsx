import { useEffect, useState } from "react";
import type { Candidate } from "../../protocol/types.ts";
import type { AppState, PairingPrompt } from "../../shared/state.ts";
import { PAIR_UNAVAILABLE, TRANSPORTS, clean } from "../../shared/text.ts";
import { act, useAction } from "../api.ts";
import { Banner, Dialog, Spinner, Switch } from "./common.tsx";
import { CheckIcon, DeviceIcon, WarningIcon } from "./icons.tsx";

const kindOf = (c: Candidate) => (c.kind === "unknown" ? "other" : c.kind);

function Signal({ rssi }: { rssi: number | null }) {
  if (rssi == null) return null;
  const bars = rssi >= -55 ? 4 : rssi >= -65 ? 3 : rssi >= -75 ? 2 : 1;
  return (
    <span className="signal" title={`Signal ${rssi} dBm`} aria-label={`Signal ${bars} of 4`}>
      {[1, 2, 3, 4].map((b) => (
        <i key={b} className={b <= bars ? "on" : ""} style={{ height: `${b * 3 + 2}px` }} />
      ))}
    </span>
  );
}

function useCountdown(expiresAt: number | undefined) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!expiresAt) return;
    const t = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(t);
  }, [expiresAt]);
  return expiresAt ? Math.max(0, Math.ceil((expiresAt - now) / 1000)) : 0;
}

function PromptView({ name, prompt }: { name: string; prompt: PairingPrompt }) {
  const [value, setValue] = useState("");
  const [busy, run] = useAction(true);
  const [error, setError] = useState<string | null>(null);
  const seconds = useCountdown(prompt.expiresAt);
  const p = prompt.prompt;
  const code = (p.value ?? "").split("").join(" ");
  // A prompt that is no longer waiting goes away with its view; other failures stay here.
  const reply = async (accept: boolean) => {
    setError(null);
    const result = await run({ type: "pair.reply", accept, value: accept ? value : undefined });
    if (!result.ok) setError(result.message);
  };
  const problem = error ? <p className="error-text">{error}</p> : null;
  if (prompt.kind === "display")
    return (
      <div className="prompt">
        <p>Type this {p.method === "pin" ? "PIN" : "code"} on {name}, then press Enter:</p>
        <div className="code">{code}</div>
        <p className="muted">{seconds} seconds left</p>
      </div>
    );
  if (p.method === "confirm_passkey")
    return (
      <div className="prompt">
        <p>Does {name} show this code?</p>
        <div className="code">{code}</div>
        <p className="muted">{seconds} seconds left</p>
        {problem}
        <div className="actions center">
          <button disabled={busy} onClick={() => void reply(false)}>
            Codes Differ
          </button>
          <button className="suggested" disabled={busy} onClick={() => void reply(true)}>
            Codes Match
          </button>
        </div>
      </div>
    );
  const passkey = p.method === "enter_passkey";
  const valid = passkey ? /^\d{6}$/.test(value) : /^[\x20-\x7e]{1,16}$/.test(value);
  return (
    <form
      className="prompt"
      onSubmit={(e) => {
        e.preventDefault();
        if (valid && !busy) void reply(true);
      }}
    >
      <p>{passkey ? `Enter the 6-digit passkey shown by ${name}:` : `Enter the PIN for ${name}:`}</p>
      <input
        autoFocus
        className="code-input"
        aria-label={passkey ? "Passkey" : "PIN"}
        inputMode={passkey ? "numeric" : "text"}
        maxLength={passkey ? 6 : 16}
        value={value}
        onChange={(e) => setValue(e.target.value)}
      />
      <p className="muted">{seconds} seconds left</p>
      {problem}
      <div className="actions center">
        <button type="button" disabled={busy} onClick={() => void reply(false)}>
          Reject
        </button>
        <button type="submit" className="suggested" disabled={busy || !valid}>
          Pair
        </button>
      </div>
    </form>
  );
}

export function AddDevice({
  state,
  open,
  onClose,
  onOpenDevice,
}: {
  state: AppState;
  open: boolean;
  onClose: () => void;
  onOpenDevice: (key: string) => void;
}) {
  const ready = state.adapters.filter((a) => a.connection === "connected" && a.readiness === "ready");
  const [adapterId, setAdapterId] = useState<string | null>(null);
  const [unnamed, setUnnamed] = useState(false);
  const chosen = ready.find((a) => a.id === adapterId) ?? ready.find((a) => a.status?.capacity.pairing.some((p) => p.available)) ?? ready[0];
  const scan = state.scan;
  const pairing = state.pairing;
  // Why starting a search or pairing failed; cleared by the next attempt.
  const [problem, setProblem] = useState<string | null>(null);
  const [searchFailed, setSearchFailed] = useState(false);
  const attempt = async (action: Parameters<typeof act>[0]) => {
    setProblem(null);
    const result = await act(action, true);
    if (action.type === "scan.start") setSearchFailed(!result.ok);
    if (!result.ok) setProblem(result.message);
  };

  // Keep the adapter chosen on opening, so status changes don't switch it.
  useEffect(() => {
    if (!open) setAdapterId(null);
    else if (chosen && adapterId !== chosen.id) setAdapterId(chosen.id);
  }, [open, chosen?.id]);

  // Search while the dialog is open and no pairing is running.
  useEffect(() => {
    if (!open || !chosen || pairing) return;
    if (scan?.adapterId === chosen.id) return;
    void attempt({ type: "scan.start", adapterId: chosen.id });
  }, [open, chosen?.id, pairing === null]);

  useEffect(() => {
    if (open && !chosen) onClose();
  }, [open, chosen]);

  const close = () => {
    if (pairing?.phase === "pairing") void act({ type: "pair.cancel" }, true);
    void act({ type: "scan.stop" }, true);
    void act({ type: "pair.dismiss" }, true);
    setProblem(null);
    setSearchFailed(false);
    onClose();
  };

  const again = () => {
    void act({ type: "pair.dismiss" }, true);
    if (chosen) void attempt({ type: "scan.start", adapterId: chosen.id });
  };

  const cancelPairing = () => {
    void act({ type: "pair.cancel" }, true);
  };

  const candidates = (scan?.adapterId === chosen?.id ? (scan?.candidates ?? []) : []).filter((c) => unnamed || clean(c.name ?? "") || c.kind !== "unknown");
  const hiddenCount = (scan?.candidates.length ?? 0) - candidates.length;
  const availability = (c: Candidate) => chosen?.status?.capacity.pairing.find((p) => p.transport === c.transport);

  let body: React.ReactNode;
  if (pairing) {
    const name = pairing.name;
    body = (
      <div className="pairing">
        {pairing.phase === "pairing" && !pairing.prompt ? (
          <div className="progress">
            <Spinner />
            <p>Pairing with {name}…</p>
          </div>
        ) : null}
        {pairing.phase === "pairing" && pairing.prompt ? <PromptView key={pairing.prompt.prompt.prompt_id} name={name} prompt={pairing.prompt} /> : null}
        {pairing.phase === "connecting" ? (
          <div className="progress">
            <Spinner />
            <p>Paired. Connecting to {name}…</p>
          </div>
        ) : null}
        {pairing.phase === "connected" ? (
          <div className="result ok">
            <CheckIcon size={40} />
            <p>{name} is paired and connected.</p>
          </div>
        ) : null}
        {pairing.phase === "saved" ? (
          <div className="result">
            <CheckIcon size={40} />
            <p>{name} is paired.</p>
            <p className="muted">{pairing.message}</p>
          </div>
        ) : null}
        {pairing.phase === "cancelled" ? (
          <div className="result">
            <p className="muted">{pairing.message}</p>
          </div>
        ) : pairing.phase === "failed" ? (
          <div className="result bad">
            <WarningIcon size={40} />
            <p>Couldn't add {name}.</p>
            <p className="muted">{pairing.message}</p>
          </div>
        ) : null}
        <footer className="dialog-footer">
          {pairing.phase === "pairing" ? <button onClick={cancelPairing}>Cancel Pairing</button> : null}
          {pairing.phase === "failed" || pairing.phase === "cancelled" ? <button onClick={again}>{pairing.phase === "failed" ? "Retry" : "Refresh"}</button> : null}
          {pairing.phase === "connected" || pairing.phase === "saved" ? (
            <>
              <button onClick={again}>Add Another</button>
              <button
                className="suggested"
                onClick={() => {
                  if (pairing.deviceKey) onOpenDevice(pairing.deviceKey);
                  close();
                }}
              >
                Done
              </button>
            </>
          ) : null}
          {pairing.phase === "failed" || pairing.phase === "cancelled" ? (
            <button className="suggested" onClick={close}>
              Close
            </button>
          ) : null}
        </footer>
      </div>
    );
  } else {
    body = (
      <>
        {ready.length > 1 ? (
          <label className="field">
            Adapter
            <select value={chosen?.id} onChange={(e) => setAdapterId(e.target.value)}>
              {ready.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}
                </option>
              ))}
            </select>
          </label>
        ) : null}
        <div className="search-status">
          {scan?.running ? (
            <>
              <Spinner /> Searching…
            </>
          ) : (problem ?? scan?.error) ? (
            <Banner kind="error" action={<button onClick={again}>{searchFailed || scan?.error ? "Retry" : "Refresh"}</button>}>
              {problem ?? scan?.error}
            </Banner>
          ) : (
            <button onClick={again}>Refresh</button>
          )}
        </div>
        <ul className="candidates" aria-label="Nearby devices">
          {candidates.map((c) => {
            const a = availability(c);
            return (
              <li key={c.candidate_id} className="candidate">
                <DeviceIcon kind={kindOf(c)} />
                <span className="side-text">
                  <span className="side-title">{clean(c.name ?? "") || "Unnamed Device"}</span>
                  <span className="side-subtitle">{TRANSPORTS[c.transport]}</span>
                </span>
                <Signal rssi={c.rssi} />
                {a && !a.available ? (
                  <span className="muted small">{a.reason ? PAIR_UNAVAILABLE[a.reason] : "Can't Add Now"}</span>
                ) : (
                  <button className="suggested" onClick={() => chosen && void attempt({ type: "pair.start", adapterId: chosen.id, candidateId: c.candidate_id })}>
                    Pair
                  </button>
                )}
              </li>
            );
          })}
          {candidates.length === 0 ? (
            <li className="candidate empty-row">
              {scan?.running ? "Looking for devices in pairing mode…" : "No Devices Found"}
            </li>
          ) : null}
        </ul>
        <div className="check">
          <span>
            Show Unnamed Devices
            {hiddenCount > 0 && !unnamed ? <span className="muted"> ({hiddenCount} hidden)</span> : null}
          </span>
          <Switch label="Show Unnamed Devices" checked={unnamed} onChange={setUnnamed} />
        </div>
        <footer className="dialog-footer">
          <button onClick={close}>Cancel</button>
        </footer>
      </>
    );
  }
  return (
    <Dialog open={open} title="Add Device" onClose={close} className="add-device">
      {body}
    </Dialog>
  );
}
