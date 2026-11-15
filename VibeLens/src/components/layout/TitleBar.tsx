import { Moon, PanelBottom, PanelLeft, PanelRight, Plus, FileCode2, Settings2, Sun } from "lucide-react";
import { useUIStore } from "../../store/useUIStore";
import { useTabActions } from "../../hooks/useTabActions";
import { Button, IconButton } from "../ui";
import { Kbd } from "../ui/wsKit";
import { MOD } from "../ui/platform";
import { cn } from "../../lib/cn";

export function TitleBar() {
  const sidebarOpen = useUIStore((s) => s.sidebarOpen);
  const agentOpen = useUIStore((s) => s.agentPanelOpen);
  const aiMode = useUIStore((s) => s.aiMode);
  const theme = useUIStore((s) => s.theme);
  const toggleSidebar = useUIStore((s) => s.toggleSidebar);
  const toggleAgent = useUIStore((s) => s.toggleAgentPanel);
  const setTheme = useUIStore((s) => s.setTheme);
  const openWizard = useUIStore((s) => s.openWizard);
  const setSettingsOpen = useUIStore((s) => s.setSettingsOpen);
  const { newSqlTab } = useTabActions();

  return (
    <header className="flex h-11 shrink-0 items-center gap-2 border-b border-border bg-panel px-3">
      <div className="flex items-center gap-2 pr-2">
        <img src="/vibelens.svg" alt="" className="size-5" />
        <span className="text-[13px] font-semibold tracking-tight">VibeLens</span>
      </div>
      <div className="h-5 w-px bg-border" aria-hidden />
      <Button size="sm" variant="secondary" onClick={() => openWizard()}>
        <Plus className="size-3.5" /> New connection
      </Button>
      <Button size="sm" variant="ghost" onClick={() => newSqlTab()} title={`New SQL tab (${MOD}+N)`}>
        <FileCode2 className="size-3.5" /> New SQL tab
        <Kbd className="ml-1">{MOD} N</Kbd>
      </Button>

      <div className="ml-auto flex items-center gap-0.5">
        <IconButton label="Settings" onClick={() => setSettingsOpen(true)}>
          <Settings2 className="size-4" />
        </IconButton>
        <IconButton label={theme === "dark" ? "Switch to light theme" : "Switch to dark theme"} onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
          {theme === "dark" ? <Sun className="size-4" /> : <Moon className="size-4" />}
        </IconButton>
        <div className="mx-1 h-5 w-px bg-border" aria-hidden />
        <IconButton label={`${sidebarOpen ? "Hide" : "Show"} navigator (${MOD}+B)`} aria-pressed={sidebarOpen} onClick={toggleSidebar} className={cn(sidebarOpen && "text-fg")}>
          <PanelLeft className="size-4" />
        </IconButton>
        {aiMode !== "classic" && (
        <IconButton label={`${agentOpen ? "Hide" : "Show"} agent panel (${MOD}+J)`} aria-pressed={agentOpen} onClick={() => toggleAgent()} className={cn(agentOpen && "text-fg")}>
          {aiMode === "agentFirst" ? <PanelRight className="size-4" /> : <PanelBottom className="size-4" />}
        </IconButton>
        )}
      </div>
    </header>
  );
}
