import { Group, Panel, Separator, useDefaultLayout } from "react-resizable-panels";
import { useUIStore } from "../../store/useUIStore";
import { AgentTerminal } from "../agent/AgentTerminal";
import { Sidebar } from "../sidebar/Sidebar";
import { WorkArea } from "./WorkArea";
import { ErrorBoundary } from "./ErrorBoundary";
import { layoutStorage, panelStyle } from "./storage";

/**
 * Three-column wireframe from the design doc:
 *   navigator | (tabs + editor / results) over a collapsible agent panel.
 */
export function Shell() {
  const sidebarOpen = useUIStore((s) => s.sidebarOpen);
  const aiMode = useUIStore((s) => s.aiMode);
  // Not before the first-run choice either (it would call agent_* behind the chooser).
  const agentOpen = useUIStore((s) => s.agentPanelOpen) && aiMode !== "classic" && aiMode !== null;
  // Agent-first puts the agent in a column beside the work area; assist keeps it below.
  const agentBeside = aiMode === "agentFirst";

  const mainLayout = useDefaultLayout({ id: "vg-main", panelIds: sidebarOpen ? ["sidebar", "center"] : ["center"], storage: layoutStorage });
  const centerLayout = useDefaultLayout({ id: agentBeside ? "vg-center-beside" : "vg-center", panelIds: agentOpen ? ["work", "agent"] : ["work"], storage: layoutStorage });

  return (
    <div className="min-h-0 flex-1">
      <Group orientation="horizontal" defaultLayout={mainLayout.defaultLayout} onLayoutChanged={mainLayout.onLayoutChanged} className="h-full">
        {sidebarOpen && (
          <>
            <Panel style={panelStyle} id="sidebar" defaultSize="260px" minSize="200px" maxSize="520px" groupResizeBehavior="preserve-pixel-size">
              <ErrorBoundary scope="navigator">
                <Sidebar />
              </ErrorBoundary>
            </Panel>
            <Separator className="vg-sep" />
          </>
        )}
        <Panel style={panelStyle} id="center" minSize="480px">
          <Group key={agentBeside ? "beside" : "below"} orientation={agentBeside ? "horizontal" : "vertical"} defaultLayout={centerLayout.defaultLayout} onLayoutChanged={centerLayout.onLayoutChanged} className="h-full">
            <Panel style={panelStyle} id="work" minSize={agentBeside ? 360 : 160}>
              <ErrorBoundary scope="workspace">
                <WorkArea />
              </ErrorBoundary>
            </Panel>
            {agentOpen && (
              <>
                <Separator className="vg-sep" />
                <Panel style={panelStyle} id="agent" defaultSize={agentBeside ? "46%" : "32%"} minSize={agentBeside ? 340 : 120} maxSize="75%">
                  <div className="h-full min-h-0 bg-panel">
                    <ErrorBoundary scope="agent panel">
                      <AgentTerminal />
                    </ErrorBoundary>
                  </div>
                </Panel>
              </>
            )}
          </Group>
        </Panel>
      </Group>
    </div>
  );
}
