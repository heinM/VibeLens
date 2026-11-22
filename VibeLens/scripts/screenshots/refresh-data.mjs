#!/usr/bin/env node
// Copies the result rows the screenshot scenes show from the seeded demo databases into
// data/*.json, so the pictures show real rows and the capture itself needs no database.
//
// Needs the dev containers from docker-compose.dev.yml (scripts/dev-mac.sh starts them), seeded
// from scripts/seed/. Read-only: every statement here is a SELECT.
//
//   node scripts/screenshots/refresh-data.mjs
//
// The Iceberg rows come from replaying scripts/seed/iceberg-seed.py's random sequence in Python
// (no catalog needed): see iceberg-rows.py.
import { execFileSync } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "data");
mkdirSync(out, { recursive: true });

const US = "\u001f";

function psql(sql) {
  const text = execFileSync("docker", ["exec", "vibelens-postgres-1", "psql", "-U", "postgres", "-d", "vibelens_test", "-At", "-F", US, "-c", sql], { encoding: "utf8" });
  return text.split("\n").filter((l) => l.length > 0).map((l) => l.split(US));
}

function mysql(sql) {
  const text = execFileSync("docker", ["exec", "vibelens-mysql-1", "mysql", "-uroot", "-pvibelens", "shop", "-N", "-B", "-e", sql], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  return text.split("\n").filter((l) => l.length > 0).map((l) => l.split("\t"));
}

// Cells as the Rust codecs send them (crates/vibelens-core/src/pg/codec.rs, mysql/cells.rs):
// int8 / numeric / bigint / decimal as strings, int4 as numbers, timestamptz as RFC 3339 in UTC,
// json parsed, NULL as null.
const pgTs = (s) => s.replace(" ", "T").replace(/\+00$/, "Z");
function pgCell(type, v) {
  if (v === "") return null;
  switch (type) {
    case "int4":
      return Number(v);
    case "timestamptz":
      return pgTs(v);
    default:
      return v; // int8, numeric, text, bpchar, enum: text
  }
}

// MySQL text protocol: BIT(n) as n binary digits, JSON parsed, NULL as null, dates as sent.
function myCell(type, v) {
  if (v === "NULL") return null;
  switch (type) {
    case "bit":
      return v;
    case "json":
      return JSON.parse(v);
    default:
      return v;
  }
}

function pgResult(sql, columns) {
  const rows = psql(sql).map((r) => r.map((v, i) => pgCell(columns[i].typeName, v)));
  return { sql, columns, rows };
}

const PG = { int8: 20, int4: 23, text: 25, bpchar: 1042, numeric: 1700, timestamptz: 1184 };
const col = (name, typeName, typeOid = PG[typeName] ?? 0) => ({ name, typeName, typeOid });

const topCustomers = pgResult(
  "SELECT c.id, c.full_name, c.country, count(o.id) AS orders, sum(o.total) AS spent, max(o.placed_at) AS last_order " +
    "FROM shop.customers c JOIN shop.orders o ON o.customer_id = c.id WHERE o.status <> 'cancelled' " +
    "GROUP BY c.id, c.full_name, c.country ORDER BY spent DESC LIMIT 25",
  [col("id", "int8"), col("full_name", "text"), col("country", "bpchar"), col("orders", "int8"), col("spent", "numeric"), col("last_order", "timestamptz")],
);

const openOrders = pgResult("SELECT * FROM shop.v_open_orders ORDER BY placed_at DESC LIMIT 200", [
  col("id", "int8"),
  col("email", "text"),
  // An enum's type name is its own (`order_status`); the oid is per database.
  col("status", "order_status", 16411),
  col("total", "numeric"),
  col("placed_at", "timestamptz"),
]);

const stalePending = pgResult("SELECT count(*) FROM shop.orders WHERE status = 'pending' AND placed_at < now() - interval '14 days'", [col("count", "int8")]);

const ordersByStatus = pgResult(
  "SELECT status, count(*) AS orders, sum(total) AS revenue FROM shop.orders GROUP BY status ORDER BY revenue DESC",
  [col("status", "order_status", 16411), col("orders", "int8"), col("revenue", "numeric")],
);

const [version] = psql("SHOW server_version")[0];

// MySQL: the BIT column is read as its binary digits, padded to the column width (4).
const myColumns = [
  { name: "id", typeName: "bigint unsigned", typeOid: 8 },
  { name: "full_name", typeName: "varchar", typeOid: 253 },
  { name: "country", typeName: "char", typeOid: 254 },
  { name: "last_login", typeName: "date", typeOid: 10 },
  { name: "flags", typeName: "bit", typeOid: 16 },
  { name: "profile", typeName: "json", typeOid: 245 },
];
const myRows = mysql("SELECT id, full_name, country, last_login, LPAD(BIN(flags), 4, '0'), profile FROM customers ORDER BY id").map((r) =>
  r.map((v, i) => myCell(myColumns[i].typeName, v)),
);
const [myVersion] = mysql("SELECT version()")[0];

writeFileSync(
  join(out, "postgres.json"),
  JSON.stringify({ serverVersion: version, topCustomers, openOrders, stalePending, ordersByStatus }, null, 1) + "\n",
);
writeFileSync(
  join(out, "mysql.json"),
  JSON.stringify(
    { serverVersion: myVersion, customers: { sql: "SELECT id, full_name, country, last_login, flags, profile FROM customers ORDER BY id;", columns: myColumns, rows: myRows } },
    null,
    1,
  ) + "\n",
);

const ice = execFileSync("python3", ["-I", join(here, "iceberg-rows.py")], { encoding: "utf8" });
writeFileSync(join(out, "iceberg.json"), ice);
console.log("wrote", out);
