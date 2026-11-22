import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useUIStore } from "./store/useUIStore";
import { PROFILES_KEY } from "./hooks/useProfiles";
import { STATUS_KEY } from "./hooks/useConnectionStatus";
import { useGlobalShortcuts } from "./hooks/useGlobalShortcuts";
import { ConnectionWizard } from "./components/connections/ConnectionWizard";
import { ImportDialog } from "./components/connections/ImportDialog";
import { HostKeyPrompt } from "./components/connections/HostKeyDialog";
import { SettingsDialog } from "./components/settings/SettingsDialog";
import { Shell } from "./components/layout/Shell";
import { StatusBar } from "./components/layout/StatusBar";
import { TitleBar } from "./components/layout/TitleBar";
import { readAiMode, readBool, readTheme, writeAiMode, writeBool, writeTheme } from "./components/layout/storage";
import { ModeChooser } from "./components/layout/ModeChooser";
import { CommandPalette } from "./components/palette/CommandPalette";
import { CloseGuard } from "./components/layout/CloseGuard";
import { useAgentStore } from "./store/useAgentStore";
import { api } from "./ipc/commands";
import "./components/layout/layout.css";

// Restore persisted shell state before first paint so there is no theme flash.
(function bootstrapShell() {
  const ui = useUIStore.getState();
  ui.setTheme(readTheme());
  const mode = readAiMode();
  if (mode) ui.setAiMode(mode);
  if (!readBool("vg.sidebarOpen", true) && ui.sidebarOpen) ui.toggleSidebar();
  if (!readBool("vg.agentOpen", true) && ui.agentPanelOpen) ui.toggleAgentPanel(false);
})();

export default function App() {
  const qc = useQueryClient();
  const wizardOpen = useUIStore((s) => s.wizard.open);
  useGlobalShortcuts();

  // Persist theme and panel visibility (never throws; see layout/storage).
  useEffect(
    () =>
      useUIStore.subscribe((s, prev) => {
        if (s.theme !== prev.theme) writeTheme(s.theme);
        if (s.aiMode && s.aiMode !== prev.aiMode) writeAiMode(s.aiMode);
        // Classic mode unmounts the agent panel: don't leave a turn running (and calling tools) unseen.
        if (s.aiMode === "classic" && prev.aiMode !== "classic") {
          const agent = useAgentStore.getState();
          if (agent.running) {
            const sid = agent.sessionId;
            void api
              .agentCancel(sid)
              .then(() => {
                if (useAgentStore.getState().sessionId === sid) useAgentStore.getState().dispatch({ type: "cancelled" });
              })
              .catch(() => {});
          }
        }
        if (s.sidebarOpen !== prev.sidebarOpen) writeBool("vg.sidebarOpen", s.sidebarOpen);
        if (s.agentPanelOpen !== prev.agentPanelOpen) writeBool("vg.agentOpen", s.agentPanelOpen);
      }),
    [],
  );

  // When the wizard closes a profile was probably saved or deleted: refresh lists and statuses.
  const wasOpen = useRef(false);
  useEffect(() => {
    const closed = wasOpen.current && !wizardOpen;
    wasOpen.current = wizardOpen;
    if (!closed) return;
    void qc.invalidateQueries({ queryKey: PROFILES_KEY });
    void qc.invalidateQueries({ queryKey: STATUS_KEY });
  }, [wizardOpen, qc]);

  return (
    <div className="flex h-full flex-col bg-bg text-fg">
      <TitleBar />
      <Shell />
      <StatusBar />
      <ConnectionWizard />
      <ImportDialog />
      <HostKeyPrompt />
      <SettingsDialog />
      <ModeChooser />
      <CommandPalette />
      <CloseGuard />
    </div>
  );
}
