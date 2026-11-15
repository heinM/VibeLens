// Ephemeral UI layout state (Zustand). Shared contract between frontend modules.
import { create } from "zustand";

/**
 * How much AI the interface shows. The engines behave the same in every mode; this only
 * decides whether and where the agent appears.
 * - classic:    a normal database client. No agent panel, no AI controls.
 * - assist:     classic layout plus a collapsible agent panel at the bottom.
 * - agentFirst: the agent is a primary column beside the editor, open by default.
 */
export type AiMode = "classic" | "assist" | "agentFirst";

/** True when AI features (agent panel, inline AI, "Fix with AI", plan explanations) should show. */
export const selectAiEnabled = (s: { aiMode: AiMode | null }) => s.aiMode === "assist" || s.aiMode === "agentFirst";

interface UIState {
  sidebarOpen: boolean;
  agentPanelOpen: boolean;
  /** Profile the user is "focused" on (sidebar selection); default target for new tabs + agent. */
  activeProfileId: string | null;
  activeDatabase: string | null;
  wizard: { open: boolean; editingProfileId: string | null };
  settingsOpen: boolean;
  /** Import connections from DataGrip, DBeaver, Azure Data Studio, pgAdmin, pg_service.conf. */
  importOpen: boolean;
  theme: "dark" | "light";
  /** null until the user picks one on first launch. */
  aiMode: AiMode | null;

  toggleSidebar: () => void;
  toggleAgentPanel: (open?: boolean) => void;
  setActiveTarget: (profileId: string | null, database?: string | null) => void;
  openWizard: (editingProfileId?: string | null) => void;
  closeWizard: () => void;
  setSettingsOpen: (open: boolean) => void;
  setImportOpen: (open: boolean) => void;
  setTheme: (t: "dark" | "light") => void;
  setAiMode: (mode: AiMode) => void;
}

export const useUIStore = create<UIState>((set) => ({
  sidebarOpen: true,
  agentPanelOpen: true,
  activeProfileId: null,
  activeDatabase: null,
  wizard: { open: false, editingProfileId: null },
  settingsOpen: false,
  importOpen: false,
  theme: "dark",
  aiMode: null,

  toggleSidebar: () => set((s) => ({ sidebarOpen: !s.sidebarOpen })),
  // The agent panel can't be opened in classic mode.
  toggleAgentPanel: (open) => set((s) => ({ agentPanelOpen: s.aiMode === "classic" ? false : (open ?? !s.agentPanelOpen) })),
  setAiMode: (aiMode) => set({ aiMode, agentPanelOpen: aiMode !== "classic" }),
  setActiveTarget: (activeProfileId, database = null) => set({ activeProfileId, activeDatabase: database }),
  openWizard: (editingProfileId = null) => set({ wizard: { open: true, editingProfileId } }),
  closeWizard: () => set({ wizard: { open: false, editingProfileId: null } }),
  setSettingsOpen: (settingsOpen) => set({ settingsOpen }),
  setImportOpen: (importOpen) => set({ importOpen }),
  setTheme: (theme) => {
    document.documentElement.classList.toggle("dark", theme === "dark");
    document.documentElement.classList.toggle("light", theme === "light");
    set({ theme });
  },
}));
