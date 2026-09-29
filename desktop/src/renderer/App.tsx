import { useEffect, useRef, useState } from "react";
import type { Navigation } from "../shared/state.ts";
import { act, api, setReporter, useAppState } from "./api.ts";
import { AddDevice } from "./components/AddDevice.tsx";
import { AdapterPage } from "./components/AdapterPage.tsx";
import { DevicePage } from "./components/DevicePage.tsx";
import { HomePage } from "./components/HomePage.tsx";
import { Preferences } from "./components/Preferences.tsx";
import { Sidebar } from "./components/Sidebar.tsx";

export type Selection = { page: "home" } | { page: "device"; key: string } | { page: "adapter"; id: string };

const HOME: Selection = { page: "home" };

export function App() {
  const state = useAppState();
  const [selection, setSelection] = useState<Selection>(HOME);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [preferences, setPreferences] = useState(false);
  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    setReporter(setToast);
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
  const watched = shown.page === "device" ? shown.key : null;
  const shownId = selection.page === "device" ? selection.key : selection.page === "adapter" ? selection.id : null;
  const seen = useRef<string | null>(null);

  useEffect(() => {
    if (current) seen.current = shownId;
    else if (seen.current === shownId) setSelection(HOME);
  }, [current, shownId]);

  useEffect(() => {
    void act({ type: "settings.watch", key: watched });
  }, [watched]);

  if (!state) return <div className="loading" />;

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
          <DevicePage key={shown.key} state={state} entry={devices.find((d) => d.key === shown.key)!} onAdd={() => setAdding(true)} />
        ) : shown.page === "adapter" ? (
          <AdapterPage
            key={shown.id}
            state={state}
            adapter={adapters.find((a) => a.id === shown.id)!}
            renaming={renaming === shown.id}
            onRenamed={() => setRenaming(null)}
            onSelect={setSelection}
          />
        ) : (
          <HomePage state={state} onSelect={setSelection} onAdd={() => setAdding(true)} />
        )}
      </main>
      <AddDevice state={state} open={adding} onClose={() => setAdding(false)} onOpenDevice={(key) => setSelection({ page: "device", key })} />
      <Preferences state={state} open={preferences} onClose={() => setPreferences(false)} />
      {toast ? (
        <div className="toast" role="alert" onClick={() => setToast(null)}>
          {toast}
        </div>
      ) : null}
    </div>
  );
}
