import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import type { AdapterChanges, DeviceChanges, Navigation } from "../shared/state.ts";
import { api, setReporter, useAppState } from "./api.ts";
import { AddDevice } from "./components/AddDevice.tsx";
import { AdapterPage } from "./components/AdapterPage.tsx";
import { DevicePage, type Draft, type Drafts } from "./components/DevicePage.tsx";
import { HomePage } from "./components/HomePage.tsx";
import { Preferences } from "./components/Preferences.tsx";
import { Sidebar } from "./components/Sidebar.tsx";

export type Selection = { page: "home" } | { page: "device"; key: string } | { page: "adapter"; id: string };

const HOME: Selection = { page: "home" };

/** Updates the staged changes kept for `id`, forgetting them once none are left. */
const stager = <T extends object>(set: Dispatch<SetStateAction<Record<string, T>>>, id: string) =>
  (update: (draft: T) => T) =>
    set((all) => {
      const next = { ...all };
      const draft = update(all[id] ?? ({} as T));
      if (Object.keys(draft).length) next[id] = draft;
      else delete next[id];
      return next;
    });

export function App() {
  const state = useAppState();
  const [selection, setSelection] = useState<Selection>(HOME);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [preferences, setPreferences] = useState(false);
  // Each report is a new toast, so the same message again shows for its full time.
  const [toast, setToast] = useState<{ message: string; id: number } | null>(null);
  // Staged setting changes by device and setting, kept across tabs and pages.
  const [drafts, setDrafts] = useState<Record<string, Draft>>({});
  // Staged Details changes by device, kept across tabs and pages.
  const [details, setDetails] = useState<Record<string, DeviceChanges>>({});
  // Staged adapter settings by adapter, kept across tabs and pages.
  const [adapterDrafts, setAdapterDrafts] = useState<Record<string, AdapterChanges>>({});

  useEffect(() => {
    let reports = 0;
    setReporter((message) => setToast({ message, id: ++reports }));
    return api.onNavigate((to: Navigation) => {
      if (to.page === "hidden") {
        setAdding(false);
        setPreferences(false);
      } else if (to.page === "add-device") setAdding(true);
      else if (to.page === "preferences") setPreferences(true);
      else if (to.page === "device") setSelection({ page: "device", key: to.key });
      else {
        setSelection({ page: "adapter", id: to.id });
        if (to.rename) setRenaming(to.id);
      }
    });
  }, []);

  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 6000);
    return () => clearTimeout(t);
  }, [toast]);

  // Show the home page while the selection isn't in the state. A navigation
  // may arrive before the state that contains its target, so the selection
  // returns home only once a target that was shown disappears.
  const devices = state?.devices ?? [];
  const adapters = state?.adapters ?? [];
  const current =
    selection.page === "device"
      ? devices.find((d) => d.key === selection.key)
      : selection.page === "adapter"
        ? adapters.find((a) => a.id === selection.id)
        : true;
  const shown = current ? selection : HOME;
  const shownId = selection.page === "device" ? selection.key : selection.page === "adapter" ? selection.id : null;
  const seen = useRef<string | null>(null);

  useEffect(() => {
    if (current) seen.current = shownId;
    else if (seen.current === shownId) setSelection(HOME);
  }, [current, shownId]);

  if (!state) return <div className="loading" />;

  const draftsFor = (key: string): Drafts => ({
    get: (setting) => drafts[`${key} ${setting}`],
    set: (setting, draft, expected) =>
      setDrafts((all) => {
        const id = `${key} ${setting}`;
        if (expected !== undefined && all[id] !== expected) return all;
        const next = { ...all };
        if (draft === undefined) delete next[id];
        else next[id] = draft;
        return next;
      }),
    clear: () => setDrafts((all) => Object.fromEntries(Object.entries(all).filter(([id]) => !id.startsWith(`${key} `)))),
  });

  return (
    <div className="app">
      <Sidebar
        state={state}
        selection={shown}
        onSelect={setSelection}
        onAdd={() => setAdding(true)}
        onPreferences={() => setPreferences(true)}
        onRename={(id) => {
          setSelection({ page: "adapter", id });
          setRenaming(id);
        }}
      />
      <main className="content">
        {shown.page === "device" ? (
          <DevicePage
            key={shown.key}
            state={state}
            entry={devices.find((d) => d.key === shown.key)!}
            drafts={draftsFor(shown.key)}
            details={details[shown.key] ?? {}}
            onDetails={stager(setDetails, shown.key)}
          />
        ) : shown.page === "adapter" ? (
          <AdapterPage
            key={shown.id}
            state={state}
            adapter={adapters.find((a) => a.id === shown.id)!}
            renaming={renaming === shown.id}
            draft={adapterDrafts[shown.id] ?? {}}
            onDraft={stager(setAdapterDrafts, shown.id)}
            onRenamed={() => setRenaming(null)}
            onSelect={setSelection}
          />
        ) : (
          <HomePage state={state} onSelect={setSelection} />
        )}
      </main>
      <AddDevice state={state} open={adding} onClose={() => setAdding(false)} onOpenDevice={(key) => setSelection({ page: "device", key })} />
      <Preferences state={state} open={preferences} onClose={() => setPreferences(false)} />
      {toast ? (
        <div key={toast.id} className="toast" role="alert" onClick={() => setToast(null)}>
          {toast.message}
        </div>
      ) : null}
    </div>
  );
}
