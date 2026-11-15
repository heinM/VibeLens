import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../../ipc/commands";
import { useWorkspaceStore, type WorkspaceTab } from "../../store/useWorkspaceStore";
import { ConfirmDialog } from "../ui";

const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/**
 * Unsaved-work protection, UI side.
 * - Reports the number of dirty tabs to Rust, which guards window close and ⌘Q with a native dialog
 *   (see `src-tauri/src/lifecycle.rs`; it works even if this UI has crashed).
 * - ⌘W (macOS menu → `vg://menu` "close-tab") / Ctrl+W elsewhere closes the active tab, asking first
 *   when it has unsaved changes. It never closes the window.
 */
export function CloseGuard() {
  const [closing, setClosing] = useState<WorkspaceTab | null>(null);

  useEffect(() => {
    let last = -1;
    const report = (tabs: WorkspaceTab[]) => {
      const n = tabs.reduce((acc, t) => acc + (t.dirty ? 1 : 0), 0);
      if (n === last) return;
      last = n;
      void api.appSetUnsaved(n).catch(() => {}); // not in Tauri (browser preview/tests): nothing to guard
    };
    report(useWorkspaceStore.getState().tabs);
    return useWorkspaceStore.subscribe((s) => report(s.tabs));
  }, []);

  useEffect(() => {
    const closeActive = () => {
      const ws = useWorkspaceStore.getState();
      const t = ws.activeTab();
      if (!t) return;
      if (t.dirty) setClosing(t);
      else ws.closeTab(t.id);
    };
    let unlisten: (() => void) | undefined;
    let disposed = false;
    listen<string>("vg://menu", (e) => {
      if (e.payload === "close-tab") closeActive();
    })
      .then((u) => (disposed ? u() : (unlisten = u)))
      .catch(() => {});
    // macOS gets ⌘W from the native menu (it fires before the webview sees the key).
    const onKey = (e: KeyboardEvent) => {
      if (IS_MAC || !e.ctrlKey || e.metaKey || e.altKey || e.shiftKey || e.key.toLowerCase() !== "w") return;
      e.preventDefault();
      closeActive();
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      disposed = true;
      unlisten?.();
      window.removeEventListener("keydown", onKey, true);
    };
  }, []);

  return (
    <ConfirmDialog
      open={closing !== null}
      title="Discard unsaved changes?"
      confirmText="Discard"
      danger
      onCancel={() => setClosing(null)}
      onConfirm={() => {
        if (closing) useWorkspaceStore.getState().closeTab(closing.id);
        setClosing(null);
      }}
      body={<p>“{closing?.title}” has changes that have not been saved. Closing it will lose them.</p>}
    />
  );
}
