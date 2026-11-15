import type { ColumnInfo, ForeignKeyInfo, SchemaMetadata, TableDescription } from "../../../ipc/types";
import { type Catalog, indexCatalog } from "../catalog";
import { type CompletionResult, complete } from "../complete";

let ord = 0;
const col = (name: string, dataType: string, o: Partial<ColumnInfo> = {}): ColumnInfo => ({
  name,
  dataType,
  nullable: !o.isPrimaryKey,
  default: null,
  isPrimaryKey: false,
  ordinal: ++ord,
  comment: null,
  ...o,
});
const fk = (name: string, columns: string[], referencedSchema: string, referencedTable: string, referencedColumns: string[]): ForeignKeyInfo => ({
  name,
  columns,
  referencedSchema,
  referencedTable,
  referencedColumns,
});
const table = (schema: string, name: string, columns: ColumnInfo[], foreignKeys: ForeignKeyInfo[] = [], o: Partial<TableDescription> = {}): TableDescription => ({
  schema,
  name,
  kind: "table",
  comment: null,
  estimatedRows: 1000,
  columns,
  indexes: [],
  foreignKeys,
  ...o,
});

export const META: SchemaMetadata = {
  database: "shop_db",
  serverVersion: "16.4",
  truncated: false,
  tables: [
    table(
      "shop",
      "customers",
      [col("id", "int8", { isPrimaryKey: true }), col("email", "text", { nullable: false, comment: "Login email" }), col("name", "text"), col("created_at", "timestamptz")],
      [],
      { estimatedRows: 50_000, comment: "People who buy things" },
    ),
    table(
      "shop",
      "orders",
      [col("id", "int8", { isPrimaryKey: true }), col("customer_id", "int8", { nullable: false }), col("status", "text"), col("total", "numeric"), col("created_at", "timestamptz"), col("order", "int4")],
      [fk("orders_customer_id_fkey", ["customer_id"], "shop", "customers", ["id"])],
      { estimatedRows: 1_250_000 },
    ),
    table(
      "shop",
      "order_items",
      [col("order_id", "int8", { nullable: false }), col("product_id", "int8", { nullable: false }), col("qty", "int4"), col("unit_price", "numeric")],
      [fk("oi_order_fkey", ["order_id"], "shop", "orders", ["id"]), fk("oi_product_fkey", ["product_id"], "shop", "products", ["id"])],
    ),
    table("shop", "products", [col("id", "int8", { isPrimaryKey: true }), col("sku", "text"), col("name", "text"), col("price", "numeric")]),
    table("public", "audit_log", [col("id", "int8", { isPrimaryKey: true }), col("at", "timestamptz"), col("Weird Col", "text")]),
    table("public", "accounts", [col("id", "int8", { isPrimaryKey: true }), col("parent_id", "int8")], [fk("acc_parent", ["parent_id"], "public", "accounts", ["id"])]),
    table("public", "invoices", [col("id", "int8", { isPrimaryKey: true }), col("account_id", "int8"), col("amount", "numeric")]),
    table("shop", "v_open_orders", [col("id", "int8"), col("status", "text")], [], { kind: "view", estimatedRows: null }),
    table("shop", "seq_thing", [], [], { kind: "sequence" }),
  ],
};

export const CAT: Catalog = indexCatalog(META);

/** Splits `before|after` into text and cursor offset. */
export function cur(src: string): { text: string; offset: number } {
  const i = src.indexOf("|");
  if (i < 0) throw new Error("no cursor marker");
  return { text: src.slice(0, i) + src.slice(i + 1), offset: i };
}

export function run(src: string, cat: Catalog | null = CAT, triggerCharacter?: string): CompletionResult | null {
  const { text, offset } = cur(src);
  return complete(cat, text, offset, { triggerCharacter });
}

export const labels = (r: CompletionResult | null): string[] => (r ? r.suggestions.map((s) => s.label) : []);
export const sorted = (r: CompletionResult | null): string[] =>
  r ? [...r.suggestions].sort((a, b) => (a.sortText < b.sortText ? -1 : a.sortText > b.sortText ? 1 : 0)).map((s) => s.label) : [];
