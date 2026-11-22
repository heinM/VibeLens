// The screenshots, and the starting state each one needs before the app starts. What happens after
// start-up (which connection is focused, which tab runs what) is in scenes.ts.
export interface SceneSetup {
  /** PNG written to docs/images/. */
  file: string;
  aiMode?: "classic" | "assist" | "agentFirst";
  agentOpen?: boolean;
  /** Navigator width, % of the window. */
  sidebarPct?: number;
  /** Navigator sections (connections / explorer / files and history), % of its height. */
  sidebarRows?: { profiles: number; explorer: number; files: number };
  /** Agent panel, % of the centre column (height in assist, width in agent-first). */
  agentPct?: number;
  /** Editor share of a SQL tab, % (the rest is results). */
  editorPct?: number;
  /** Connections the stand-in reports as connected (default: all but Billing and Athena). */
  connected?: string[];
  /** false: Lakehouse (Athena) is plainly disconnected instead of showing an expired SSO session. */
  athenaExpired?: boolean;
}

const LIST = {
  overview: { file: "overview.png", agentPct: 30, editorPct: 36 },
  postgres: { file: "postgres-partitions.png", agentOpen: false, sidebarPct: 47, sidebarRows: { profiles: 35, explorer: 59, files: 6 }, editorPct: 32 },
  mysql: { file: "mysql.png", agentOpen: false, sidebarPct: 22, sidebarRows: { profiles: 32, explorer: 56, files: 12 }, editorPct: 34 },
  valkey: { file: "valkey-inspector.png", agentOpen: false, sidebarRows: { profiles: 32, explorer: 56, files: 12 } },
  iceberg: { file: "iceberg-trino.png", agentOpen: false, sidebarPct: 22, sidebarRows: { profiles: 32, explorer: 56, files: 12 }, editorPct: 40 },
  icebergEngines: { file: "iceberg-engines.png", agentOpen: false },
  redshift: { file: "redshift.png", agentOpen: false, sidebarPct: 22, sidebarRows: { profiles: 32, explorer: 56, files: 12 }, editorPct: 38 },
  newConnection: { file: "new-connection.png", agentOpen: false },
  aiAccess: { file: "connection-ai-access.png", agentOpen: false },
  agent: { file: "agent-run-in-tab.png", aiMode: "agentFirst", agentPct: 46, editorPct: 34 },
  approval: { file: "agent-approval.png", aiMode: "agentFirst", agentPct: 50, editorPct: 40 },
  providers: { file: "settings-providers.png", agentOpen: false },
  sso: { file: "sso-sign-in-again.png", agentOpen: false, sidebarRows: { profiles: 32, explorer: 52, files: 16 } },
} satisfies Record<string, SceneSetup>;

export type SceneName = keyof typeof LIST;
export const SCENES: Record<SceneName, SceneSetup> = LIST;
