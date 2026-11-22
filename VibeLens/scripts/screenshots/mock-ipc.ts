// A stand-in for the Tauri backend, for screenshots only. It answers the commands in
// src/ipc/commands.ts with demo data of the exact return types, and streams QueryEvents and
// AgentEvents through the same Channel mechanism the Rust side uses. It is installed with
// @tauri-apps/api's own mockIPC, so the application code runs unchanged.
//
// A command it does not know is refused (and logged to window.__mockLog), never answered with a
// guess, so a scene cannot silently show something the real backend would not.
import { mockIPC } from "@tauri-apps/api/mocks";
import type * as T from "../../src/ipc/types";
import {
  ICE_FIELDS,
  ICE_NAMESPACES,
  ICE_TABLES,
  ID,
  MY_DATABASES,
  MY_OBJECTS,
  MY_TABLES,
  ORDER_EVENTS_LISTING,
  PG_DATABASES,
  PG_OBJECTS,
  PG_SCHEMAS,
  PG_TABLES,
  PROFILES,
  PROVIDERS,
  SERVER_VERSION,
  SSO_EXPIRED_DETAIL,
  WORKSPACE,
  ice,
  my,
  pg,
  valkeyKeys,
  valkeyValue,
  type Result,
} from "./demo-data";

type Args = Record<string, unknown>;
interface ChannelLike {
  id: number;
}

declare global {
  interface Window {
    __mockLog: string[];
    __TAURI_INTERNALS__: { runCallback: (id: number, data: unknown) => void };
  }
}

/** What a scene changes about the backend before the app starts. */
export const mockState = {
  connected: new Set<string>(),
  /** Profiles whose last connect failed: their status is `error` with this detail. */
  failed: new Map<string, string>(),
  /** Which scripted agent turn `agent_start_turn` plays (picked by the prompt when null). */
  appVersion: "0.2.0-2",
};

// ---------------------------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------------------------

const sent = new Map<number, number>();
function emit(ch: ChannelLike, message: unknown) {
  const index = sent.get(ch.id) ?? 0;
  sent.set(ch.id, index + 1);
  window.__TAURI_INTERNALS__.runCallback(ch.id, { index, message });
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------------------------

const norm = (sql: string) => sql.replace(/--[^\n]*(\n|$)/g, "").replace(/;\s*$/, "").replace(/\s+/g, " ").trim().toLowerCase();

interface Answer {
  result: Result;
  stats?: Omit<Extract<T.QueryEvent, { type: "stats" }>, "type" | "statementIndex">;
  elapsedMs: number;
}

/** Redshift reports its own type names for the same columns (redshift/cells.rs `display_type`). */
const redshiftTop: Result = {
  ...pg.topCustomers,
  columns: [
    { name: "id", typeName: "int8", typeOid: 0 },
    { name: "full_name", typeName: "varchar", typeOid: 0 },
    { name: "country", typeName: "bpchar", typeOid: 0 },
    { name: "orders", typeName: "int8", typeOid: 0 },
    { name: "spent", typeName: "numeric(38,2)", typeOid: 0 },
    { name: "last_order", typeName: "timestamptz", typeOid: 0 },
  ],
};

const ANSWERS: Record<T.EngineKind, Map<string, Answer>> = {
  postgres: new Map([
    [norm(pg.topCustomers.sql), { result: pg.topCustomers, elapsedMs: 41 }],
    [norm(pg.openOrders.sql), { result: pg.openOrders, elapsedMs: 18 }],
    [norm(pg.stalePending.sql), { result: pg.stalePending, elapsedMs: 9 }],
    [norm(pg.ordersByStatus.sql), { result: pg.ordersByStatus, elapsedMs: 23 }],
  ]),
  mysql: new Map([[norm(my.customers.sql), { result: my.customers, elapsedMs: 6 }]]),
  iceberg: new Map([
    [
      norm(ice.revenueByCountry.sql),
      {
        result: ice.revenueByCountry,
        // remote/trino.rs: engine "trino", processed bytes from the query stats, Trino's query id.
        stats: { engine: "trino", bytesScanned: 61_872, costUsdEstimate: null, engineQueryId: "20261122_081455_00137_k7r2v" },
        elapsedMs: 1840,
      },
    ],
  ]),
  redshift: new Map([
    [
      norm(pg.topCustomers.sql),
      {
        result: redshiftTop,
        stats: { engine: "redshift", bytesScanned: null, costUsdEstimate: null, engineQueryId: "3c1f6a52-8d4e-4b0f-9f6e-2a7d81c0b9e4" },
        elapsedMs: 2210,
      },
    ],
  ]),
  valkey: new Map(),
};

let querySeq = 0;
function execute(engine: T.EngineKind, args: Args): string {
  const queryId = `q-${++querySeq}`;
  const ch = args.onEvent as ChannelLike;
  const answer = ANSWERS[engine].get(norm(String(args.sql)));
  void (async () => {
    await sleep(40);
    if (!answer) {
      window.__mockLog.push(`unknown ${engine} SQL: ${String(args.sql)}`);
      emit(ch, { type: "started", queryId, statementCount: 1 });
      emit(ch, { type: "failed", statementIndex: 0, error: { code: "other", message: "The screenshot stand-in has no result for this statement." } });
      return;
    }
    const { result, stats, elapsedMs } = answer;
    emit(ch, { type: "started", queryId, statementCount: 1 });
    emit(ch, { type: "columns", statementIndex: 0, columns: result.columns });
    for (let i = 0; i < result.rows.length; i += 500) emit(ch, { type: "rows", statementIndex: 0, rows: result.rows.slice(i, i + 500) });
    if (stats) emit(ch, { type: "stats", statementIndex: 0, ...stats });
    emit(ch, { type: "statementDone", statementIndex: 0, commandTag: "SELECT", rowsAffected: null, rowCount: result.rows.length, truncated: false, elapsedMs });
    emit(ch, { type: "finished", elapsedMs: elapsedMs + 3 });
  })();
  return queryId;
}

// ---------------------------------------------------------------------------------------------
// Agent turns: real AgentEvent sequences, as session.rs emits them for these tool calls
// ---------------------------------------------------------------------------------------------

const SHOP = PROFILES.find((p) => p.id === ID.shopPg) as T.ConnectionProfile;

export const AGENT_TOP_SQL = `SELECT c.id, c.full_name, c.country,
       count(o.id) AS orders,
       sum(o.total) AS spent,
       max(o.placed_at) AS last_order
FROM shop.customers c
JOIN shop.orders o ON o.customer_id = c.id
WHERE o.status <> 'cancelled'
GROUP BY c.id, c.full_name, c.country
ORDER BY spent DESC
LIMIT 25;`;

const STALE_SQL = `SELECT count(*) FROM shop.orders WHERE status = 'pending' AND placed_at < now() - interval '14 days';`;
const CANCEL_SQL = `UPDATE shop.orders
SET status = 'cancelled'
WHERE status = 'pending'
  AND placed_at < now() - interval '14 days';`;

type Step = { wait: number; ev: T.AgentEvent };

function money(s: unknown): string {
  const n = Number(s);
  return n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

function topCustomersTurn(): Step[] {
  const top = pg.topCustomers.rows.slice(0, 5);
  const table = [
    "| Customer | Country | Orders | Spent |",
    "|---|---|---:|---:|",
    ...top.map((r) => `| ${r[1]} (#${r[0]}) | ${r[2]} | ${r[3]} | ${money(r[4])} |`),
  ].join("\n");
  const run: T.SqlRun = { profileId: SHOP.id, database: "vibelens_test", sql: AGENT_TOP_SQL, readOnly: true };
  return [
    { wait: 60, ev: { type: "turnStarted", turnId: "t-1", providerId: "anthropic", model: "claude-sonnet-5-5" } },
    { wait: 80, ev: { type: "textDelta", text: "I'll check the `shop` schema first, then add up each customer's orders that were not cancelled." } },
    { wait: 60, ev: { type: "toolCallProposed", callId: "c1", tool: "get_schema", arguments: { schema: "shop" }, risk: "safe", reason: null } },
    { wait: 20, ev: { type: "toolStarted", callId: "c1" } },
    { wait: 60, ev: { type: "toolFinished", callId: "c1", ok: true, summary: "7 tables", elapsedMs: 38, sqlRun: null } },
    { wait: 40, ev: { type: "toolCallProposed", callId: "c2", tool: "describe_table", arguments: { schema: "shop", table: "orders" }, risk: "safe", reason: null } },
    { wait: 20, ev: { type: "toolStarted", callId: "c2" } },
    { wait: 40, ev: { type: "toolFinished", callId: "c2", ok: true, summary: "shop.orders (6 columns)", elapsedMs: 21, sqlRun: null } },
    {
      wait: 60,
      ev: {
        type: "textDelta",
        text: "\n\n`orders.customer_id` references `customers.id`, and `status` is the `order_status` enum, so cancelled orders are easy to leave out.",
      },
    },
    {
      wait: 60,
      ev: {
        type: "toolCallProposed",
        callId: "c3",
        tool: "run_sql",
        arguments: { sql: AGENT_TOP_SQL, purpose: "Top customers by spend, excluding cancelled orders" },
        risk: "safe",
        reason: null,
      },
    },
    { wait: 20, ev: { type: "toolStarted", callId: "c3" } },
    { wait: 80, ev: { type: "toolFinished", callId: "c3", ok: true, summary: `${pg.topCustomers.rows.length} rows`, elapsedMs: 64, sqlRun: run } },
    {
      wait: 60,
      ev: {
        type: "textDelta",
        text: `\n\nThe top five by spend:\n\n${table}\n\nSeveral customers tie because the demo data repeats its patterns. Use **Run in tab** on the query above to see all ${pg.topCustomers.rows.length} rows in the grid.`,
      },
    },
    { wait: 40, ev: { type: "turnFinished", stopReason: "end_turn", usage: { inputTokens: 9412, outputTokens: 486 } } },
  ];
}

function cancelStaleTurn(): Step[] {
  const n = Number(pg.stalePending.rows[0][0]);
  const reason = `This changes data on 'Shop', database 'vibelens_test': 1 statement: UPDATE. Purpose: Cancel orders that have been pending for more than 14 days`;
  const args = { sql: CANCEL_SQL, purpose: "Cancel orders that have been pending for more than 14 days" };
  return [
    { wait: 60, ev: { type: "turnStarted", turnId: "t-2", providerId: "anthropic", model: "claude-sonnet-5-5" } },
    { wait: 60, ev: { type: "textDelta", text: "First, how many orders would this touch:" } },
    { wait: 40, ev: { type: "toolCallProposed", callId: "c4", tool: "run_sql", arguments: { sql: STALE_SQL, purpose: "Count stale pending orders" }, risk: "safe", reason: null } },
    { wait: 20, ev: { type: "toolStarted", callId: "c4" } },
    {
      wait: 60,
      ev: { type: "toolFinished", callId: "c4", ok: true, summary: "1 row", elapsedMs: 12, sqlRun: { profileId: SHOP.id, database: "vibelens_test", sql: STALE_SQL, readOnly: true } },
    },
    {
      wait: 60,
      ev: {
        type: "textDelta",
        text: `\n\n**${n.toLocaleString("en-US")}** orders have been pending for more than 14 days. This \`UPDATE\` marks them cancelled; it runs only if you approve it.`,
      },
    },
    { wait: 60, ev: { type: "toolCallProposed", callId: "c5", tool: "run_sql", arguments: args, risk: "needsApproval", reason } },
    {
      wait: 40,
      ev: {
        type: "approvalRequired",
        callId: "c5",
        tool: "run_sql",
        arguments: args,
        reason,
        preview: CANCEL_SQL,
        target: { connection: "Shop", engine: "postgres", environment: "dev", database: "vibelens_test", defaultDatabase: "vibelens_test", valkeyDb: null, detail: null },
      },
    },
  ];
}

let turnSeq = 0;
function startTurn(args: Args): string {
  const ch = args.onEvent as ChannelLike;
  const prompt = String(args.prompt).toLowerCase();
  const steps = /pending/.test(prompt) ? cancelStaleTurn() : topCustomersTurn();
  void (async () => {
    for (const s of steps) {
      await sleep(s.wait);
      emit(ch, s.ev);
    }
  })();
  return `turn-${++turnSeq}`;
}

// ---------------------------------------------------------------------------------------------
// Status, catalogs
// ---------------------------------------------------------------------------------------------

const NOW = "2026-11-22T08:10:00Z";

function status(p: T.ConnectionProfile): T.ConnectionStatus {
  if (mockState.connected.has(p.id))
    return { profileId: p.id, engine: p.engine, state: "connected", since: NOW, serverVersion: SERVER_VERSION[p.id] ?? null, detail: null, tunneled: false };
  const detail = mockState.failed.get(p.id);
  if (detail) return { profileId: p.id, engine: p.engine, state: "error", since: NOW, serverVersion: null, detail, tunneled: false };
  return { profileId: p.id, engine: p.engine, state: "disconnected", since: null, serverVersion: null, detail: null, tunneled: false };
}

function profileOf(args: Args): T.ConnectionProfile {
  const p = PROFILES.find((x) => x.id === args.profileId);
  if (!p) throw { code: "not_found", message: `not found: profile ${String(args.profileId)}` };
  return p;
}

const pgMeta = (): T.SchemaMetadata => ({
  database: "vibelens_test",
  serverVersion: pg.serverVersion,
  tables: Object.values(PG_TABLES),
  truncated: false,
});

function iceInfo(namespace: string[], name: string): T.IcebergTableInfo {
  const key = `${namespace.join(".")}.${name}`;
  const def = ICE_FIELDS[key] ?? { fields: [{ id: 1, name: "id", typeName: "long", required: true, doc: null, children: [] }], spec: [] };
  const t0 = Date.UTC(2026, 10, 20, 9, 30, 0);
  const snaps: T.IcebergSnapshot[] =
    key === "shop.orders"
      ? [
          { snapshotId: "6211407635104598187", parentSnapshotId: "2879410733851902635", sequenceNumber: "3", timestampMs: t0 + 4200, operation: "overwrite", summary: { "added-records": "3000", "deleted-records": "3500" }, manifestList: null, schemaId: 0 },
          { snapshotId: "2879410733851902635", parentSnapshotId: "8476921330715240001", sequenceNumber: "2", timestampMs: t0 + 2100, operation: "append", summary: { "added-records": "1500" }, manifestList: null, schemaId: 0 },
          { snapshotId: "8476921330715240001", parentSnapshotId: null, sequenceNumber: "1", timestampMs: t0, operation: "append", summary: { "added-records": "2000" }, manifestList: null, schemaId: 0 },
        ]
      : [{ snapshotId: "4410972315563309112", parentSnapshotId: null, sequenceNumber: "1", timestampMs: t0, operation: "append", summary: {}, manifestList: null, schemaId: 0 }];
  return {
    namespace,
    name,
    kind: "table",
    location: `s3://example-lake/warehouse/${namespace.join("/")}/${name}`,
    metadataLocation: null,
    formatVersion: 2,
    tableUuid: "0d6f3b8e-5c1a-4e2f-9b7d-3a8c6e1f2d40",
    lastUpdatedMs: snaps[0].timestampMs,
    currentSnapshotId: snaps[0].snapshotId,
    currentSchemaId: 0,
    fields: def.fields,
    schemaCount: 1,
    partitionSpec: def.spec,
    sortOrder: [],
    properties: { "write.format.default": "parquet" },
    snapshots: snaps,
    refs: [{ name: "main", snapshotId: snaps[0].snapshotId, kind: "branch" }],
  };
}

const RS_SCHEMAS = ["information_schema", "pg_catalog", "public", "shop"];
const RS_TABLES: T.RedshiftTableRef[] = ["customers", "order_items", "orders", "products"].map((name) => ({ schema: "shop", name, kind: "TABLE" }));
const rsc = (name: string, typeName: string, nullable: boolean, length: number | null = null, precision: number | null = null, scale: number | null = null): T.RedshiftColumn => ({
  name,
  typeName,
  nullable,
  length,
  precision,
  scale,
  defaultValue: null,
});
const RS_COLUMNS: Record<string, T.RedshiftColumn[]> = {
  customers: [rsc("id", "int8", false), rsc("email", "varchar", false, 256), rsc("full_name", "varchar", false, 256), rsc("country", "bpchar", false, 2), rsc("created_at", "timestamptz", false)],
  orders: [
    rsc("id", "int8", false),
    rsc("customer_id", "int8", false),
    rsc("status", "varchar", false, 16),
    rsc("total", "numeric", false, null, 14, 2),
    rsc("placed_at", "timestamptz", false),
    rsc("notes", "varchar", true, 256),
  ],
  order_items: [rsc("order_id", "int8", false), rsc("product_id", "int8", false), rsc("qty", "int4", false), rsc("unit_price", "numeric", false, null, 12, 2)],
  products: [rsc("id", "int8", false), rsc("sku", "varchar", false, 32), rsc("name", "varchar", false, 256), rsc("price", "numeric", false, null, 12, 2), rsc("attributes", "super", false)],
};

function globToRegex(glob: string): RegExp {
  const esc = glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
  return new RegExp(`^${esc}$`);
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

const HANDLERS: Record<string, (a: Args) => unknown> = {
  app_info: (): T.AppInfo => ({
    version: mockState.appVersion,
    configDir: "/Users/demo/Library/Application Support/com.heinvandermerwe.vibelens",
    workspaceDir: "/Users/demo/Documents/VibeLens",
    vaultBackend: "keychain",
  }),
  app_set_unsaved: () => null,
  sql_classify: (a): T.SqlClassification => {
    const read = /^\s*(select|with|show|explain|values|table)\b/i.test(String(a.sql));
    return read
      ? { statements: ["readOnly"], overall: "readOnly", parseOk: true, summary: "1 statement: SELECT" }
      : { statements: ["write"], overall: "write", parseOk: true, summary: "1 statement: UPDATE" };
  },

  list_profiles: () => PROFILES,
  profile_secret_status: (): T.SecretStatus => ({ hasPassword: true, hasSshPassword: false, hasSshPassphrase: false, hasIcebergSecret: true, hasS3Keys: false, hasEngineSecret: true }),
  connection_status: () => PROFILES.filter((p) => mockState.connected.has(p.id) || mockState.failed.has(p.id)).map(status),
  connect: (a) => {
    const p = profileOf(a);
    const detail = mockState.failed.get(p.id);
    if (detail) throw { code: "auth", message: detail };
    mockState.connected.add(p.id);
    return status(p);
  },
  disconnect: (a) => {
    mockState.connected.delete(String(a.profileId));
    return null;
  },

  pg_list_databases: () => PG_DATABASES,
  pg_list_schemas: () => PG_SCHEMAS,
  pg_list_objects: (a) => (a.schema === "shop" ? PG_OBJECTS : []),
  pg_list_partitions: (a) => {
    if (a.table === "order_events") return ORDER_EVENTS_LISTING;
    return { partitionKey: null, partitions: [], truncated: false, partman: null };
  },
  pg_describe_table: (a) => {
    const t = PG_TABLES[String(a.table)];
    if (t) return t;
    const part = String(a.table).startsWith("order_events_") ? PG_TABLES.order_events : null;
    if (part) return { ...part, name: String(a.table), kind: "table" };
    throw { code: "not_found", message: `not found: ${String(a.schema)}.${String(a.table)}` };
  },
  pg_schema_metadata: () => pgMeta(),
  pg_execute: (a) => execute("postgres", a),
  pg_cancel: () => null,

  my_list_databases: () => MY_DATABASES,
  my_list_objects: (a) => (a.database === "shop" ? MY_OBJECTS : []),
  my_describe_table: (a) => MY_TABLES[String(a.table)] ?? (() => { throw { code: "not_found", message: "not found" }; })(),
  my_schema_metadata: (): T.SchemaMetadata => ({ database: "shop", serverVersion: my.serverVersion, tables: Object.values(MY_TABLES), truncated: false }),
  my_execute: (a) => execute("mysql", a),

  rs_list_databases: () => ["dev"],
  rs_list_schemas: () => RS_SCHEMAS,
  rs_list_tables: (a) => (a.schema === "shop" ? RS_TABLES : []),
  rs_describe_table: (a) => RS_COLUMNS[String(a.name)] ?? [],
  rs_schema_metadata: (): T.SchemaMetadata => ({
    database: "dev",
    serverVersion: null,
    tables: Object.entries(RS_COLUMNS).map(([name, cols]) => ({
      schema: "shop",
      name,
      kind: "table",
      comment: null,
      estimatedRows: null,
      columns: cols.map((c, i) => ({ name: c.name, dataType: c.typeName, nullable: c.nullable, default: null, isPrimaryKey: false, ordinal: i + 1, comment: null })),
      indexes: [],
      foreignKeys: [],
    })),
    truncated: false,
  }),
  rs_execute: (a) => execute("redshift", a),

  ice_list_namespaces: (a) => (a.parent && (a.parent as string[]).length > 0 ? [] : ICE_NAMESPACES),
  ice_list_tables: (a) => ICE_TABLES[(a.namespace as string[]).join(".")] ?? [],
  ice_describe_table: (a) => iceInfo(a.namespace as string[], String(a.name)),
  ice_schema_metadata: (): T.SchemaMetadata => ({
    database: "lake",
    serverVersion: null,
    tables: Object.entries(ICE_FIELDS).map(([k, v]) => ({
      schema: k.split(".")[0],
      name: k.split(".")[1],
      kind: "table",
      comment: null,
      estimatedRows: null,
      columns: v.fields.map((fl, i) => ({ name: fl.name, dataType: fl.typeName, nullable: !fl.required, default: null, isPrimaryKey: false, ordinal: i + 1, comment: null })),
      indexes: [],
      foreignKeys: [],
    })),
    truncated: false,
  }),
  ice_execute: (a) => execute("iceberg", a),

  vk_server_info: (): T.ValkeyServerInfo => ({ version: "8.1.12", mode: "standalone", role: "master", usedMemoryHuman: "3.21M", extra: {} }),
  vk_list_dbs: (): T.ValkeyDbInfo[] => Array.from({ length: 16 }, (_, index) => ({ index, keys: index === 0 ? valkeyKeys().length : 0 })),
  vk_scan: (a): T.ScanPage => {
    const re = globToRegex(String(a.pattern ?? "*"));
    return { keys: a.db === 0 ? valkeyKeys().filter((k) => re.test(k.key)) : [], cursor: "0", done: true };
  },
  vk_get_value: (a) => {
    const v = valkeyValue(String(a.key));
    if (!v) throw { code: "not_found", message: "not found: key" };
    return v;
  },

  aws_list_profiles: (): T.AwsProfileSummary[] => [],
  aws_sso_sessions: (): T.SsoSessionInfo[] => [{ startUrl: "https://example.awsapps.com/start", region: "eu-west-1", expiresAt: "2026-11-22T06:00:00Z" }],

  agent_list_providers: () => PROVIDERS,
  agent_start_turn: (a) => startTurn(a),
  agent_resolve_approval: () => null,
  agent_cancel: () => null,
  agent_reset: () => null,

  ws_list: (): T.WorkspaceFile[] => Object.entries(WORKSPACE).map(([path, text]) => ({ path, size: text.length, modified: "2026-11-21T16:40:00Z" })),
  ws_read: (a) => {
    const text = WORKSPACE[String(a.path)];
    if (text === undefined) throw { code: "not_found", message: "not found: file" };
    return text;
  },
};

export function installMock(): void {
  window.__mockLog = [];
  mockIPC(
    (cmd, payload) => {
      const h = HANDLERS[cmd];
      if (!h) {
        window.__mockLog.push(`unhandled command: ${cmd}`);
        return Promise.reject({ code: "other", message: `${cmd} is not available in the screenshot stand-in` });
      }
      try {
        return Promise.resolve(h((payload ?? {}) as Args));
      } catch (e) {
        return Promise.reject(e);
      }
    },
    { shouldMockEvents: true },
  );
}
