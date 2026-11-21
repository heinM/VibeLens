// Opening agent-written SQL (or Valkey commands) in an editor tab. The target is given by the
// caller: the agent's current target for a fenced block, or what Rust reported for a run_sql call.
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { requestAutoRun } from "../editor/autoRun";

export interface AgentTabRequest {
  kind: "sql" | "valkey";
  profileId: string | null;
  /** Postgres / Redshift database; ignored for Valkey. */
  database: string | null;
  contents: string;
  /** Run it once the tab's editor is mounted, through the tab's normal run path. */
  run?: boolean;
}

/** Opens a new unsaved tab and returns its id. */
export function openAgentTab({ kind, profileId, database, contents, run = false }: AgentTabRequest): string {
  const sql = kind === "sql";
  const id = useWorkspaceStore.getState().openTab({
    kind,
    contents,
    profileId,
    database: sql ? database : null,
    title: sql ? "agent.sql" : "agent.valkey",
    dirty: true,
  });
  if (run && sql) requestAutoRun(id);
  return id;
}
