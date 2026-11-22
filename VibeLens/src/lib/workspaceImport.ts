// "Import files into workspace": the shared flow behind the Workspace files button and the
// palette command. Rust shows the picker and does every path check; this only reacts to the result.
import { create } from "zustand";
import type { QueryClient } from "@tanstack/react-query";
import { WS_FILES_KEY } from "../hooks/useWorkspaceFiles";
import { api, asError } from "../ipc/commands";
import type { ErrorPayload, ImportSkip, WorkspaceImportOutcome } from "../ipc/types";
import { useWorkspaceStore } from "../store/useWorkspaceStore";

/** What the Workspace files panel shows after an import: skipped files with reasons, or a failure. */
export interface ImportNotice {
  imported: number;
  skipped: ImportSkip[];
  error: ErrorPayload | null;
}

interface ImportNoticeState {
  notice: ImportNotice | null;
  setNotice: (n: ImportNotice | null) => void;
}

export const useImportNoticeStore = create<ImportNoticeState>((set) => ({ notice: null, setNotice: (notice) => set({ notice }) }));

export interface ImportDeps {
  pick: () => Promise<WorkspaceImportOutcome | null>;
  refreshList: () => void;
  openFile: (path: string) => Promise<unknown>;
  setNotice: (n: ImportNotice | null) => void;
}

/** Runs one import. Never throws: failures become a notice. A single imported file opens in a tab. */
export async function runWorkspaceImport(deps: ImportDeps): Promise<void> {
  let outcome: WorkspaceImportOutcome | null;
  try {
    outcome = await deps.pick();
  } catch (e) {
    deps.setNotice({ imported: 0, skipped: [], error: asError(e) });
    return;
  }
  if (!outcome) return; // dialog cancelled: leave any earlier notice alone
  deps.refreshList();
  let error: ErrorPayload | null = null;
  if (outcome.imported.length === 1) {
    try {
      await deps.openFile(outcome.imported[0]);
    } catch (e) {
      error = asError(e);
    }
  }
  const noisy = outcome.skipped.length > 0 || error !== null;
  deps.setNotice(noisy ? { imported: outcome.imported.length, skipped: outcome.skipped, error } : null);
}

/** The real thing, wired to the app. */
export function importWorkspaceFiles(qc: QueryClient, profileId: string | null): Promise<void> {
  return runWorkspaceImport({
    pick: api.wsImportFiles,
    refreshList: () => void qc.invalidateQueries({ queryKey: WS_FILES_KEY }),
    openFile: (path) => useWorkspaceStore.getState().openWorkspaceFile(path, profileId),
    setNotice: useImportNoticeStore.getState().setNotice,
  });
}
