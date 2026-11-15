import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useUIStore } from "../store/useUIStore";
import { useWorkspaceStore } from "../store/useWorkspaceStore";
import type { ConnectionProfile } from "../ipc/types";
import { PROFILES_KEY } from "./useProfiles";
import { useTabActions } from "./useTabActions";
import { schemaMetadataKey } from "../lib/schemaCache";
import { resolveErdTarget, schemasByTableCount } from "../components/palette/erdTarget";
import { usePaletteStore } from "../components/palette/paletteStore";

const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
import { openErdTab } from "../components/table/openTableTab";

/**
 * App-wide shortcuts:
 * ⌘/Ctrl+N new SQL tab, ⌘/Ctrl+B navigator, ⌘/Ctrl+J agent panel (not in classic mode),
 * ⌘/Ctrl+P go to anything, ⌘/Ctrl+⇧+P commands, ⌘/Ctrl+⇧+E ER diagram of the active schema.
 */
export function useGlobalShortcuts() {
  const { newSqlTab } = useTabActions();
  const qc = useQueryClient();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // ⌘ on macOS, Ctrl elsewhere: Ctrl+N/P/B are emacs-style cursor keys on macOS (Monaco uses them).
      const mod = IS_MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
      if (!mod || e.altKey) return;
      const key = e.key.toLowerCase();
      const ui = useUIStore.getState();

      if (e.shiftKey) {
        // Shifted shortcuts get the capture phase so Monaco and the grid never swallow them.
        if (key === "p") {
          e.preventDefault();
          usePaletteStore.getState().show("commands");
        } else if (key === "e") {
          e.preventDefault();
          const profiles = qc.getQueryData<ConnectionProfile[]>(PROFILES_KEY);
          const target = resolveErdTarget({
            activeTab: useWorkspaceStore.getState().activeTab(),
            activeProfileId: ui.activeProfileId,
            activeDatabase: ui.activeDatabase,
            profiles,
            schemasOf: (pid, db) => {
              const fallbackDb = profiles?.find((p) => p.id === pid)?.database ?? null;
              const meta = qc.getQueryData<{ tables: { schema: string }[] }>(schemaMetadataKey(pid, db)) ?? qc.getQueryData<{ tables: { schema: string }[] }>(schemaMetadataKey(pid, fallbackDb));
              return schemasByTableCount(meta?.tables);
            },
          });
          if (target) openErdTab(target.profileId, target.database, target.schema);
          else usePaletteStore.getState().show("go"); // nothing to draw yet: let the user pick a connection
        }
        return;
      }

      switch (key) {
        case "p":
          e.preventDefault();
          usePaletteStore.getState().show("go");
          break;
        case "n":
          e.preventDefault();
          newSqlTab();
          break;
        case "b":
          e.preventDefault();
          ui.toggleSidebar();
          break;
        case "j":
          if (ui.aiMode === "classic") return; // no agent in classic mode
          e.preventDefault();
          ui.toggleAgentPanel();
          break;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [newSqlTab, qc]);
}
