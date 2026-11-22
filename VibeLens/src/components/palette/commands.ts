// The "Commands" list (⌘⇧P). Pure builder: everything it needs arrives in `env`.
import type { ConnectionProfile, ConnectionState, EngineKind } from "../../ipc/types";
import type { AiMode } from "../../store/useUIStore";
import { AI_MODES } from "../layout/aiModes";
import type { ErdTarget } from "./erdTarget";
import type { PaletteItem } from "./items";

/** Engine named on the Connect / Disconnect rows. */
const ENGINE_PALETTE_META: Record<EngineKind, string> = { postgres: "PostgreSQL", iceberg: "Iceberg", redshift: "Redshift", mysql: "MySQL", valkey: "Valkey" };

export interface CommandActions {
  newSqlTab: () => void;
  newValkeyConsole: () => void;
  newConnection: () => void;
  /** Opens the importer (DataGrip, DBeaver, Azure Data Studio, pgAdmin, pg_service.conf). */
  importConnections?: () => void;
  /** Native picker, then copies the chosen files into the workspace folder. */
  importWorkspaceFiles?: () => void;
  openSettings: () => void;
  setAiMode: (m: AiMode) => void;
  toggleTheme: () => void;
  toggleSidebar: () => void;
  toggleAgent: () => void;
  connect: (id: string) => void;
  disconnect: (id: string) => void;
  refreshSchema: () => void;
  refreshFiles: () => void;
  openErd: (t: ErdTarget) => void;
  clearHistory: () => void;
  /** Iceberg: an empty worksheet bound to this connection. */
  newIcebergQuery?: (p: ConnectionProfile) => void;
  /** Iceberg: switch the palette to a picker over this connection's tables. */
  pickIcebergTable?: (p: ConnectionProfile) => void;
}

export interface CommandEnv {
  aiMode: AiMode | null;
  theme: "dark" | "light";
  profiles: readonly ConnectionProfile[];
  stateOf: (id: string) => ConnectionState;
  /** Number of connected Postgres profiles (what "Refresh schema cache" touches). */
  connectedPg: number;
  hasValkeyProfile: boolean;
  historyCount: number;
  /** The Iceberg connection the Iceberg commands act on; null hides them (see `resolveIcebergTarget`). */
  iceberg?: ConnectionProfile | null;
  erdTarget: ErdTarget | null;
  /** Display name of the primary modifier ("⌘" or "Ctrl"). */
  mod: string;
  actions: CommandActions;
}

const aiEnabled = (m: AiMode | null) => m === "assist" || m === "agentFirst";

export function buildCommands(env: CommandEnv): PaletteItem[] {
  const { actions: a, mod } = env;
  const cmd = (id: string, group: string, title: string, rest: Partial<PaletteItem> & Pick<PaletteItem, "icon" | "run">): PaletteItem => ({
    id: `cmd:${id}`,
    group,
    title,
    search: "",
    tone: "accent",
    primary: "Run",
    ...rest,
  });
  const out: PaletteItem[] = [];

  out.push(
    cmd("new-sql", "Create", "New SQL tab", { icon: "newTab", shortcut: [mod, "N"], run: a.newSqlTab, search: "query worksheet editor" }),
    cmd("new-connection", "Create", "New connection…", { icon: "newConnection", run: a.newConnection, search: "add database postgres valkey" }),
  );
  if (a.importConnections) {
    out.push(cmd("import-connections", "Create", "Import connections…", { icon: "newConnection", run: a.importConnections, search: "import migrate datagrip dbeaver azure data studio pgadmin pg_service switch" }));
  }
  if (env.hasValkeyProfile) out.push(cmd("new-valkey", "Create", "New Valkey console", { icon: "newTab", run: a.newValkeyConsole, search: "redis cli" }));

  if (env.iceberg && a.newIcebergQuery && a.pickIcebergTable) {
    const p = env.iceberg;
    const newQuery = a.newIcebergQuery;
    const pick = a.pickIcebergTable;
    out.push(
      cmd("ice-new-query", "Iceberg", `Iceberg: new query on ${p.name || "(unnamed)"}`, { icon: "iceberg", tone: "sky", env: p.environment, run: () => newQuery(p), search: "iceberg sql worksheet editor lakehouse athena datafusion new tab" }),
      cmd("ice-open-table", "Iceberg", "Iceberg: open table…", { icon: "table", tone: "sky", meta: p.name || undefined, primary: "Choose", run: () => pick(p), search: "iceberg table explorer snapshots schema partitions browse lakehouse" }),
    );
  }

  if (env.erdTarget) {
    const t = env.erdTarget;
    out.push(cmd("erd", "Database", `Open ER diagram for ${t.schema}`, { icon: "diagram", shortcut: [mod, "⇧", "E"], run: () => a.openErd(t), search: "erd entity relationship schema diagram", tone: "amber" }));
  }
  if (env.connectedPg > 0) out.push(cmd("refresh-schema", "Database", "Refresh schema cache", { icon: "refresh", run: a.refreshSchema, search: "reload catalog tables metadata", meta: env.connectedPg > 1 ? `${env.connectedPg} connections` : undefined }));
  out.push(cmd("refresh-files", "Database", "Refresh workspace files", { icon: "refresh", run: a.refreshFiles, search: "reload worksheets" }));
  if (a.importWorkspaceFiles) {
    out.push(cmd("import-workspace-files", "Database", "Import files into workspace", { icon: "file", run: a.importWorkspaceFiles, search: "add copy bring worksheets sql file folder open" }));
  }

  for (const p of env.profiles) {
    const st = env.stateOf(p.id);
    const live = st === "connected" || st === "connecting";
    out.push(
      cmd(`${live ? "disconnect" : "connect"}:${p.id}`, "Connections", `${live ? "Disconnect" : "Connect"} ${p.name || "(unnamed)"}`, {
        icon: live ? "unplug" : "plug",
        tone: live ? "muted" : "emerald",
        env: p.environment,
        meta: ENGINE_PALETTE_META[p.engine],
        search: `${p.engine === "iceberg" ? "iceberg lakehouse" : p.engine === "redshift" ? "redshift warehouse" : p.host} ${live ? "close" : "open"}`,
        run: () => (live ? a.disconnect(p.id) : a.connect(p.id)),
      }),
    );
  }

  for (const m of AI_MODES) {
    const current = env.aiMode === m.id;
    out.push(
      cmd(`mode:${m.id}`, "Interface", `Interface mode: ${m.title.replace(/ client$/, "")}`, {
        icon: "mode",
        subtitle: m.tagline,
        meta: current ? "current" : undefined,
        search: "ai assist agent classic switch",
        run: () => a.setAiMode(m.id),
      }),
    );
  }

  out.push(
    cmd("theme", "View", env.theme === "dark" ? "Switch to light theme" : "Switch to dark theme", { icon: env.theme === "dark" ? "sun" : "moon", run: a.toggleTheme, search: "toggle theme appearance colors dark light" }),
    cmd("sidebar", "View", "Toggle navigator", { icon: "sidebar", shortcut: [mod, "B"], run: a.toggleSidebar, search: "sidebar panel explorer" }),
  );
  if (aiEnabled(env.aiMode)) out.push(cmd("agent", "View", "Toggle agent panel", { icon: "agent", shortcut: [mod, "J"], run: a.toggleAgent, search: "ai assistant chat" }));
  out.push(cmd("settings", "View", "Open settings", { icon: "settings", run: a.openSettings, search: "preferences options providers" }));

  out.push(
    cmd("clear-history", "History", "Clear query history…", {
      icon: "trash",
      tone: "danger",
      meta: env.historyCount > 0 ? `${env.historyCount} ${env.historyCount === 1 ? "entry" : "entries"}` : undefined,
      run: a.clearHistory,
      search: "delete remove queries",
      primary: "Review",
    }),
  );
  return out;
}

export const COMMAND_GROUP_ORDER = ["Create", "Iceberg", "Database", "Connections", "Interface", "View", "History"] as const;
