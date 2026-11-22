// Demo connections and catalogs the stand-in backend serves. Everything here has the exact shape of
// the real command's return type (src/ipc/types.ts) and holds only what the real backend can return.
//
// Postgres, MySQL and Valkey listings are copied from the seeded dev containers (scripts/seed/);
// result rows come from data/*.json (refresh-data.mjs). Redshift, Trino and Spark have no seed:
// they serve the same `shop` tables. `shop.order_events` (a pg_partman-managed partitioned table) is
// added for the partition screenshot; the seed has none. Names, hosts and AWS accounts are fictional.
import type * as T from "../../src/ipc/types";
import { newProfile } from "../../src/lib/defaults";
import pgData from "./data/postgres.json";
import myData from "./data/mysql.json";
import iceData from "./data/iceberg.json";

export const pg = pgData as unknown as {
  serverVersion: string;
  topCustomers: Result;
  openOrders: Result;
  stalePending: Result;
  ordersByStatus: Result;
};
export const my = myData as unknown as { serverVersion: string; customers: Result };
export const ice = iceData as unknown as { revenueByCountry: Result; orderCount: number; customerCount: number };

export interface Result {
  sql: string;
  columns: T.ColumnMeta[];
  rows: T.Cell[][];
}

// ---------------------------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------------------------

const SSO: T.AwsIdentity = {
  kind: "sso",
  startUrl: "https://example.awsapps.com/start",
  ssoRegion: "eu-west-1",
  accountId: "123456789012",
  roleName: "DataEngineer",
};
const AWS: T.AwsTarget = { identity: SSO, region: "eu-west-1" };
const STAMP = "2026-11-20T09:12:00Z";

function profile(id: string, engine: T.EngineKind, patch: Partial<T.ConnectionProfile>): T.ConnectionProfile {
  return { ...newProfile(engine), id, createdAt: STAMP, updatedAt: STAMP, ...patch };
}

export const ID = {
  shopPg: "8d0c6a1e-shop-pg",
  billing: "3f9b2c7d-billing",
  shopMy: "51e4a0b9-shop-mysql",
  cache: "c2a7e913-session-cache",
  trino: "7a6d5e40-lake-trino",
  athena: "e8b1f2c3-lake-athena",
  redshift: "0b4c9d8e-warehouse",
} as const;

export const PROFILES: T.ConnectionProfile[] = [
  profile(ID.shopPg, "postgres", {
    name: "Shop",
    host: "shop-pg.example.com",
    port: 5432,
    database: "vibelens_test",
    auth: { kind: "password", username: "app" },
    tls: { mode: "verifyFull", caCertPath: null, useRdsBundle: false },
    environment: "dev",
    aiDataAccess: "results",
    color: "#5eead4",
  }),
  profile(ID.billing, "postgres", {
    name: "Billing (Aurora)",
    host: "billing-db.example.com",
    port: 5432,
    database: "billing",
    auth: { kind: "rdsIam", username: "app_ro", aws: AWS },
    tls: { mode: "verifyFull", caCertPath: null, useRdsBundle: true },
    environment: "prod",
    aiDataAccess: "schemaOnly",
  }),
  profile(ID.shopMy, "mysql", {
    name: "Shop (MySQL)",
    host: "shop-mysql.example.com",
    port: 3306,
    database: "shop",
    auth: { kind: "password", username: "vibelens" },
    tls: { mode: "verifyFull", caCertPath: null, useRdsBundle: false },
    environment: "dev",
    aiDataAccess: "schemaOnly",
  }),
  profile(ID.cache, "valkey", {
    name: "Session cache",
    host: "sessions.example.com",
    port: 6379,
    database: null,
    auth: { kind: "none" },
    tls: { mode: "verifyFull", caCertPath: null, useRdsBundle: false },
    environment: "staging",
    aiDataAccess: "schemaOnly",
  }),
  profile(ID.trino, "iceberg", {
    name: "Lakehouse (Trino)",
    host: "",
    port: 0,
    environment: "test",
    aiDataAccess: "schemaOnly",
    iceberg: {
      catalog: {
        kind: "rest",
        uri: "https://catalog.example.com/api/catalog",
        warehouse: "lake",
        auth: { kind: "oAuth2", clientId: "vibelens", scope: "PRINCIPAL_ROLE:ALL", serverUri: null },
        headers: {},
      },
      storage: { kind: "auto" },
      queryEngine: { kind: "trino", url: "https://trino.example.com:8443", catalog: "lake", user: "analyst", auth: { kind: "basic", username: "analyst" }, caCertPath: null },
      defaultNamespace: "shop",
    },
  }),
  profile(ID.athena, "iceberg", {
    name: "Lakehouse (Athena)",
    host: "",
    port: 0,
    environment: "prod",
    aiDataAccess: "schemaOnly",
    iceberg: {
      catalog: { kind: "glue", aws: AWS, catalogId: null, warehouse: null },
      storage: { kind: "auto" },
      queryEngine: { kind: "athena", workgroup: "primary", outputLocation: "s3://example-athena-results/vibelens/", aws: null },
      defaultNamespace: "shop",
    },
  }),
  profile(ID.redshift, "redshift", {
    name: "Warehouse (Redshift)",
    host: "",
    port: 0,
    database: null,
    environment: "staging",
    aiDataAccess: "schemaOnly",
    redshift: { target: { kind: "serverless", workgroupName: "analytics" }, database: "dev", aws: AWS, auth: { kind: "iamIdentity" } },
  }),
];

/** `ConnectionStatus.serverVersion` as the backend builds it (connections/mod.rs `status_of`). */
export const SERVER_VERSION: Record<string, string> = {
  [ID.shopPg]: pg.serverVersion,
  [ID.billing]: pg.serverVersion,
  [ID.shopMy]: `MySQL ${my.serverVersion}`,
  [ID.cache]: "8.1.12",
  // iceberg/handle.rs `describe`: "{catalog kind} · {place} · {engine}"
  [ID.trino]: "REST · catalog.example.com · Trino (trino.example.com:8443 · lake)",
  [ID.athena]: "Glue · eu-west-1 · Athena (primary)",
  // redshift/mod.rs `describe_target`
  [ID.redshift]: "Redshift · analytics · dev · eu-west-1 · as the IAM identity",
};

/** CoreError::Auth(SESSION_EXPIRED) as the status poll reports it. */
export const SSO_EXPIRED_DETAIL = "authentication failed: SSO session expired — sign in again";

// ---------------------------------------------------------------------------------------------
// Postgres catalog (vibelens_test, schema shop; copied from the seeded container)
// ---------------------------------------------------------------------------------------------

export const PG_DATABASES: T.DatabaseInfo[] = [
  { name: "postgres", isTemplate: false, allowConnections: true, sizeBytes: 7689907 },
  { name: "template0", isTemplate: true, allowConnections: false, sizeBytes: 7528975 },
  { name: "template1", isTemplate: true, allowConnections: true, sizeBytes: 7763635 },
  { name: "vibelens_test", isTemplate: false, allowConnections: true, sizeBytes: 25155251 },
];

export const PG_SCHEMAS: T.SchemaInfo[] = [
  { name: "information_schema", owner: "postgres", isSystem: true },
  { name: "pg_catalog", owner: "postgres", isSystem: true },
  { name: "pg_toast", owner: "postgres", isSystem: true },
  { name: "public", owner: "pg_database_owner", isSystem: false },
  { name: "shop", owner: "postgres", isSystem: false },
];

const obj = (name: string, kind: T.ObjectKind, estimatedRows: number | null, extra: Partial<T.SchemaObject> = {}): T.SchemaObject => ({
  schema: "shop",
  name,
  kind,
  comment: null,
  estimatedRows,
  signature: null,
  isPartition: false,
  parentSchema: null,
  parentName: null,
  ...extra,
});

/** shop.order_events: monthly range partitions kept by pg_partman (premake 4, retention 6 months). */
const MONTHS = ["2026-06", "2026-07", "2026-08", "2026-09", "2026-10", "2026-11", "2026-12", "2027-01", "2027-02", "2027-03"];
const EVENT_ROWS = [182_340, 190_112, 201_874, 196_503, 214_288, 151_407, 0, 0, 0, 0];
const nextMonth = (m: string) => {
  const [y, mo] = m.split("-").map(Number);
  return mo === 12 ? `${y + 1}-01` : `${y}-${String(mo + 1).padStart(2, "0")}`;
};
export const ORDER_EVENTS_PARTITIONS: T.PartitionInfo[] = [
  ...MONTHS.map((m, i) => ({
    schema: "shop",
    name: `order_events_p${m.replace("-", "")}01`,
    bound: `FOR VALUES FROM ('${m}-01 00:00:00+00') TO ('${nextMonth(m)}-01 00:00:00+00')`,
    isDefault: false,
    isPartitioned: false,
    estimatedRows: EVENT_ROWS[i],
    sizeBytes: EVENT_ROWS[i] === 0 ? 16384 : Math.round(EVENT_ROWS[i] * 131.4),
  })),
  { schema: "shop", name: "order_events_default", bound: "DEFAULT", isDefault: true, isPartitioned: false, estimatedRows: 0, sizeBytes: 16384 },
];

export const PG_OBJECTS: T.SchemaObject[] = [
  obj("customers", "table", 2000, { comment: "People who buy things" }),
  obj("order_events", "partitionedTable", null),
  ...ORDER_EVENTS_PARTITIONS.map((p) => obj(p.name, "table", p.estimatedRows, { isPartition: true, parentSchema: "shop", parentName: "order_events" })),
  obj("order_items", "table", 74625),
  obj("orders", "table", 25000),
  obj("products", "table", 200),
  obj("v_open_orders", "view", null),
  obj("mv_daily_revenue", "materializedView", 18),
  obj("customers_id_seq", "sequence", null),
  obj("orders_id_seq", "sequence", null),
  obj("products_id_seq", "sequence", null),
  obj("customer_lifetime_value", "function", null, { signature: "(p_customer bigint)" }),
];

const c = (ordinal: number, name: string, dataType: string, nullable: boolean, def: string | null = null, isPrimaryKey = false): T.ColumnInfo => ({
  name,
  dataType,
  nullable,
  default: def,
  isPrimaryKey,
  ordinal,
  comment: null,
});
const IDENTITY = "GENERATED ALWAYS AS IDENTITY";

export const PG_TABLES: Record<string, T.TableDescription> = {
  customers: {
    schema: "shop",
    name: "customers",
    kind: "table",
    comment: "People who buy things",
    estimatedRows: 2000,
    columns: [
      c(1, "id", "bigint", false, IDENTITY, true),
      c(2, "email", "text", false),
      c(3, "full_name", "text", false),
      c(4, "country", "character(2)", false, "'ZA'::bpchar"),
      c(5, "created_at", "timestamp with time zone", false, "now()"),
    ],
    indexes: [
      { name: "customers_email_key", definition: "CREATE UNIQUE INDEX customers_email_key ON shop.customers USING btree (email)", isUnique: true, isPrimary: false },
      { name: "customers_pkey", definition: "CREATE UNIQUE INDEX customers_pkey ON shop.customers USING btree (id)", isUnique: true, isPrimary: true },
    ],
    foreignKeys: [],
  },
  orders: {
    schema: "shop",
    name: "orders",
    kind: "table",
    comment: null,
    estimatedRows: 25000,
    columns: [
      c(1, "id", "bigint", false, IDENTITY, true),
      c(2, "customer_id", "bigint", false),
      c(3, "status", "shop.order_status", false, "'pending'::shop.order_status"),
      c(4, "total", "numeric(14,2)", false, "0"),
      c(5, "placed_at", "timestamp with time zone", false, "now()"),
      c(6, "notes", "text", true),
    ],
    indexes: [
      { name: "orders_customer_idx", definition: "CREATE INDEX orders_customer_idx ON shop.orders USING btree (customer_id, placed_at DESC)", isUnique: false, isPrimary: false },
      { name: "orders_pkey", definition: "CREATE UNIQUE INDEX orders_pkey ON shop.orders USING btree (id)", isUnique: true, isPrimary: true },
    ],
    foreignKeys: [{ name: "orders_customer_id_fkey", columns: ["customer_id"], referencedSchema: "shop", referencedTable: "customers", referencedColumns: ["id"] }],
  },
  order_items: {
    schema: "shop",
    name: "order_items",
    kind: "table",
    comment: null,
    estimatedRows: 74625,
    columns: [c(1, "order_id", "bigint", false, null, true), c(2, "product_id", "bigint", false, null, true), c(3, "qty", "integer", false), c(4, "unit_price", "numeric(12,2)", false)],
    indexes: [{ name: "order_items_pkey", definition: "CREATE UNIQUE INDEX order_items_pkey ON shop.order_items USING btree (order_id, product_id)", isUnique: true, isPrimary: true }],
    foreignKeys: [
      { name: "order_items_order_id_fkey", columns: ["order_id"], referencedSchema: "shop", referencedTable: "orders", referencedColumns: ["id"] },
      { name: "order_items_product_id_fkey", columns: ["product_id"], referencedSchema: "shop", referencedTable: "products", referencedColumns: ["id"] },
    ],
  },
  products: {
    schema: "shop",
    name: "products",
    kind: "table",
    comment: null,
    estimatedRows: 200,
    columns: [
      c(1, "id", "bigint", false, IDENTITY, true),
      c(2, "sku", "text", false),
      c(3, "name", "text", false),
      c(4, "price", "numeric(12,2)", false),
      c(5, "attributes", "jsonb", false, "'{}'::jsonb"),
    ],
    indexes: [
      { name: "products_pkey", definition: "CREATE UNIQUE INDEX products_pkey ON shop.products USING btree (id)", isUnique: true, isPrimary: true },
      { name: "products_sku_key", definition: "CREATE UNIQUE INDEX products_sku_key ON shop.products USING btree (sku)", isUnique: true, isPrimary: false },
    ],
    foreignKeys: [],
  },
  order_events: {
    schema: "shop",
    name: "order_events",
    kind: "partitionedTable",
    comment: null,
    estimatedRows: null,
    columns: [c(1, "order_id", "bigint", false), c(2, "kind", "text", false), c(3, "created_at", "timestamp with time zone", false, "now()"), c(4, "payload", "jsonb", true)],
    indexes: [{ name: "order_events_order_idx", definition: "CREATE INDEX order_events_order_idx ON ONLY shop.order_events USING btree (order_id, created_at)", isUnique: false, isPrimary: false }],
    foreignKeys: [],
  },
  v_open_orders: {
    schema: "shop",
    name: "v_open_orders",
    kind: "view",
    comment: null,
    estimatedRows: null,
    columns: [
      c(1, "id", "bigint", true),
      c(2, "email", "text", true),
      c(3, "status", "shop.order_status", true),
      c(4, "total", "numeric(14,2)", true),
      c(5, "placed_at", "timestamp with time zone", true),
    ],
    indexes: [],
    foreignKeys: [],
  },
  mv_daily_revenue: {
    schema: "shop",
    name: "mv_daily_revenue",
    kind: "materializedView",
    comment: null,
    estimatedRows: 18,
    columns: [c(1, "day", "timestamp with time zone", true), c(2, "orders", "bigint", true), c(3, "revenue", "numeric", true)],
    indexes: [],
    foreignKeys: [],
  },
};

export const ORDER_EVENTS_LISTING: T.PartitionListing = {
  partitionKey: "RANGE (created_at)",
  partitions: ORDER_EVENTS_PARTITIONS,
  truncated: false,
  partman: { control: "created_at", partitionInterval: "1 mon", premake: 4, retention: "6 months", retentionKeepTable: false, automaticMaintenance: "on" },
};

// ---------------------------------------------------------------------------------------------
// MySQL catalog (database shop; copied from the seeded container)
// ---------------------------------------------------------------------------------------------

export const MY_DATABASES = ["information_schema", "mysql", "performance_schema", "shop", "sys"];

const mo = (name: string, kind: T.ObjectKind, estimatedRows: number | null, comment: string | null = null): T.SchemaObject => ({
  schema: "shop",
  name,
  kind,
  comment,
  estimatedRows,
  signature: null,
  isPartition: false,
  parentSchema: null,
  parentName: null,
});
// TABLE_ROWS as InnoDB reports it for the freshly seeded tables.
export const MY_OBJECTS: T.SchemaObject[] = [
  mo("customer_totals", "view", null),
  mo("customers", "table", 4, "People who buy things"),
  mo("order_items", "table", 0),
  mo("orders", "table", 0),
  mo("products", "table", 3),
  mo("order_count", "function", null),
  mo("recent_orders", "procedure", null),
];

const myc = (ordinal: number, name: string, dataType: string, nullable: boolean, def: string | null = null, pk = false, comment: string | null = null): T.ColumnInfo => ({
  name,
  dataType,
  nullable,
  default: def,
  isPrimaryKey: pk,
  ordinal,
  comment,
});
export const MY_TABLES: Record<string, T.TableDescription> = {
  customers: {
    schema: "shop",
    name: "customers",
    kind: "table",
    comment: "People who buy things",
    estimatedRows: 4,
    columns: [
      myc(1, "id", "bigint unsigned auto_increment", false, null, true),
      myc(2, "email", "varchar(255)", false),
      myc(3, "full_name", "varchar(200)", false),
      myc(4, "country", "char(2)", false, "ZA"),
      myc(5, "created_at", "datetime", false, "CURRENT_TIMESTAMP"),
      myc(6, "last_login", "date", true, null, false, "zero date for customers who never logged in"),
      myc(7, "flags", "bit(4)", false, "b'0'"),
      myc(8, "profile", "json", true),
    ],
    indexes: [
      { name: "email", definition: "UNIQUE INDEX `email` (`email`) USING BTREE", isUnique: true, isPrimary: false },
      { name: "PRIMARY", definition: "PRIMARY KEY (`id`) USING BTREE", isUnique: true, isPrimary: true },
    ],
    foreignKeys: [],
  },
  orders: {
    schema: "shop",
    name: "orders",
    kind: "table",
    comment: null,
    estimatedRows: 0,
    columns: [
      myc(1, "id", "bigint auto_increment", false, null, true),
      myc(2, "customer_id", "bigint unsigned", false),
      myc(3, "status", "enum('new','paid','shipped','cancelled')", false, "new"),
      myc(4, "total", "decimal(65,30)", false, "0.000000000000000000000000000000"),
      myc(5, "placed_at", "timestamp", false, "CURRENT_TIMESTAMP"),
    ],
    indexes: [
      { name: "idx_orders_customer", definition: "INDEX `idx_orders_customer` (`customer_id`, `placed_at`) USING BTREE", isUnique: false, isPrimary: false },
      { name: "PRIMARY", definition: "PRIMARY KEY (`id`) USING BTREE", isUnique: true, isPrimary: true },
    ],
    foreignKeys: [{ name: "fk_orders_customer", columns: ["customer_id"], referencedSchema: "shop", referencedTable: "customers", referencedColumns: ["id"] }],
  },
  order_items: {
    schema: "shop",
    name: "order_items",
    kind: "table",
    comment: null,
    estimatedRows: 0,
    columns: [
      myc(1, "order_id", "bigint", false, null, true),
      myc(2, "product_id", "int", false, null, true),
      myc(3, "quantity", "smallint unsigned", false),
      myc(4, "unit_price", "decimal(12,2)", false),
    ],
    indexes: [
      { name: "fk_items_product", definition: "INDEX `fk_items_product` (`product_id`) USING BTREE", isUnique: false, isPrimary: false },
      { name: "PRIMARY", definition: "PRIMARY KEY (`order_id`, `product_id`) USING BTREE", isUnique: true, isPrimary: true },
    ],
    foreignKeys: [
      { name: "fk_items_order", columns: ["order_id"], referencedSchema: "shop", referencedTable: "orders", referencedColumns: ["id"] },
      { name: "fk_items_product", columns: ["product_id"], referencedSchema: "shop", referencedTable: "products", referencedColumns: ["id"] },
    ],
  },
  products: {
    schema: "shop",
    name: "products",
    kind: "table",
    comment: null,
    estimatedRows: 3,
    columns: [
      myc(1, "id", "int auto_increment", false, null, true),
      myc(2, "sku", "varchar(40)", false),
      myc(3, "name", "varchar(200)", false),
      myc(4, "price", "decimal(12,2)", false),
      myc(5, "weight_kg", "double", true),
      myc(6, "thumbnail", "blob", true),
      myc(7, "warehouse", "point", true),
    ],
    indexes: [
      { name: "PRIMARY", definition: "PRIMARY KEY (`id`) USING BTREE", isUnique: true, isPrimary: true },
      { name: "sku", definition: "UNIQUE INDEX `sku` (`sku`) USING BTREE", isUnique: true, isPrimary: false },
    ],
    foreignKeys: [],
  },
  customer_totals: {
    schema: "shop",
    name: "customer_totals",
    kind: "view",
    comment: null,
    estimatedRows: null,
    columns: [myc(1, "id", "bigint unsigned", false, "0"), myc(2, "full_name", "varchar(200)", false), myc(3, "orders", "bigint", false, "0"), myc(4, "spent", "decimal(65,30)", false, "0.000000000000000000000000000000")],
    indexes: [],
    foreignKeys: [],
  },
};

// ---------------------------------------------------------------------------------------------
// Valkey (the state right after scripts/seed/valkey-seed.sh)
// ---------------------------------------------------------------------------------------------

export function valkeyKeys(): T.KeyEntry[] {
  const keys: T.KeyEntry[] = [];
  for (let i = 1; i <= 1500; i++) keys.push({ key: `user:${i}:session`, keyType: "hash" });
  for (let i = 1; i <= 300; i++) keys.push({ key: `cart:${i}`, keyType: "list" });
  keys.push({ key: "leaderboard:weekly", keyType: "zset" });
  keys.push({ key: "feature:beta-users", keyType: "set" });
  keys.push({ key: "config:app", keyType: "string" });
  keys.push({ key: "counter:page-views", keyType: "string" });
  keys.push({ key: "events:orders", keyType: "stream" });
  return keys;
}

export function valkeyValue(key: string): T.ValkeyValue | null {
  if (key === "config:app") {
    const value = '{"theme":"dark","retries":3,"regions":["af-south-1","eu-west-1"]}';
    return { key, keyType: "string", ttlMs: -1, size: value.length, value, truncated: false };
  }
  const m = /^user:(\d+):session$/.exec(key);
  if (m) {
    const i = Number(m[1]);
    return {
      key,
      keyType: "hash",
      ttlMs: (3600 + i) * 1000 - 1_204_000,
      size: 4,
      value: { user_id: String(i), email: `customer${i}@example.com`, plan: i % 5 === 0 ? "pro" : "free", last_seen: "2026-10-31T10:00:00Z" },
      truncated: false,
    };
  }
  if (key === "leaderboard:weekly") {
    const entries = Array.from({ length: 100 }, (_, k) => [`player:${k + 1}`, String(((k + 1) * 37) % 1000)] as [string, string]);
    entries.sort((a, b) => Number(a[1]) - Number(b[1]));
    return { key, keyType: "zset", ttlMs: -1, size: 100, value: entries, truncated: false };
  }
  return null;
}

// ---------------------------------------------------------------------------------------------
// Iceberg (namespaces and tables of scripts/seed/iceberg-seed.py)
// ---------------------------------------------------------------------------------------------

export const ICE_NAMESPACES: T.IcebergNamespace[] = [
  { levels: ["history"], properties: { comment: "Tables whose history is not a straight line" } },
  { levels: ["shop"], properties: { owner: "vibelens-demo", comment: "Demo e-commerce lakehouse" } },
];
export const ICE_TABLES: Record<string, T.IcebergTableRef[]> = {
  shop: ["customers", "events", "orders"].map((name) => ({ namespace: ["shop"], name, kind: "table" as const })),
  history: ["branched", "rolled_back"].map((name) => ({ namespace: ["history"], name, kind: "table" as const })),
};

const f = (id: number, name: string, typeName: string, required: boolean, children: T.IcebergField[] = []): T.IcebergField => ({ id, name, typeName, required, doc: null, children });

export const ICE_FIELDS: Record<string, { fields: T.IcebergField[]; spec: T.IcebergPartitionField[] }> = {
  "shop.orders": {
    fields: [f(1, "id", "long", true), f(2, "customer_id", "long", true), f(3, "status", "string", false), f(4, "total", "decimal(12, 2)", false), f(5, "placed_at", "timestamptz", true)],
    spec: [{ sourceId: 5, fieldId: 1000, name: "placed_day", transform: "day" }],
  },
  "shop.customers": {
    fields: [f(1, "id", "long", true), f(2, "email", "string", true), f(3, "country", "string", false), f(4, "vip", "boolean", false)],
    spec: [],
  },
  "shop.events": {
    fields: [
      f(1, "event_id", "long", true),
      f(2, "customer_id", "long", false),
      f(3, "kind", "string", false),
      f(4, "score", "double", false),
      f(5, "ctx", "struct<device: string, app_version: int>", false, [f(6, "device", "string", false), f(7, "app_version", "int", false)]),
      f(8, "tags", "list<string>", false, [f(9, "element", "string", false)]),
      f(10, "attrs", "map<string, string>", false, [f(11, "key", "string", true), f(12, "value", "string", false)]),
    ],
    spec: [
      { sourceId: 2, fieldId: 1001, name: "customer_bucket", transform: "bucket[8]" },
      { sourceId: 3, fieldId: 1002, name: "kind", transform: "identity" },
    ],
  },
};

// ---------------------------------------------------------------------------------------------
// Agent providers (store.rs presets; the user has set a model for Anthropic and Ollama)
// ---------------------------------------------------------------------------------------------

const prov = (id: string, label: string, kind: T.ProviderKind, baseUrl: string, model: string, requiresApiKey: boolean, extra: Partial<T.ProviderConfig> = {}): T.ProviderConfig => ({
  id,
  label,
  kind,
  baseUrl,
  model,
  maxTokens: 8192,
  temperature: null,
  extraHeaders: {},
  requiresApiKey,
  portkey: null,
  contextWindow: null,
  ...extra,
});

export const PROVIDERS: T.ProviderSummary[] = [
  { config: prov("anthropic", "Anthropic", "anthropic", "https://api.anthropic.com", "claude-sonnet-5-5", true), hasApiKey: true },
  { config: prov("xai", "xAI (Grok)", "openAiCompatible", "https://api.x.ai/v1", "", true), hasApiKey: false },
  { config: prov("gemini", "Google Gemini", "openAiCompatible", "https://generativelanguage.googleapis.com/v1beta/openai", "", true), hasApiKey: false },
  { config: prov("ollama", "Ollama (local)", "openAiCompatible", "http://localhost:11434/v1", "qwen3:14b", false, { maxTokens: 4096, contextWindow: 16384 }), hasApiKey: false },
  {
    config: prov("portkey", "Portkey AI Gateway", "portkey", "https://api.portkey.ai/v1", "", true, {
      portkey: { apiFormat: "chatCompletions", routing: { kind: "modelCatalog" }, user: null },
    }),
    hasApiKey: false,
  },
];

// ---------------------------------------------------------------------------------------------
// Workspace files
// ---------------------------------------------------------------------------------------------

export const TOP_CUSTOMERS = `-- Best customers by spend, cancelled orders left out
SELECT c.id, c.full_name, c.country,
       count(o.id) AS orders,
       sum(o.total) AS spent,
       max(o.placed_at) AS last_order
FROM shop.customers c
JOIN shop.orders o ON o.customer_id = c.id
WHERE o.status <> 'cancelled'
GROUP BY c.id, c.full_name, c.country
ORDER BY spent DESC
LIMIT 25;`;

export const WORKSPACE: Record<string, string> = {
  "reports/top-customers.sql": TOP_CUSTOMERS,
  "migrations/2026_11_20_order_events_partman.sql":
    "-- Monthly partitions for shop.order_events, kept by pg_partman.\nSELECT partman.create_parent(\n  p_parent_table => 'shop.order_events',\n  p_control      => 'created_at',\n  p_interval     => '1 month',\n  p_premake      => 4\n);\n",
  "scratch.sql": "SELECT now();\n",
};
