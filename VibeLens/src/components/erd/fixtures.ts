// Test/fixture helpers: build TableDescription catalogs without a database.
import type { ColumnInfo, ForeignKeyInfo, TableDescription } from "../../ipc/types";

export function col(name: string, dataType = "bigint", opts: Partial<ColumnInfo> = {}): ColumnInfo {
  return { name, dataType, nullable: false, default: null, isPrimaryKey: false, ordinal: 0, comment: null, ...opts };
}

export function fk(columns: string[], referencedTable: string, referencedColumns = ["id"], schema = "shop", name?: string): ForeignKeyInfo {
  return { name: name ?? `${columns.join("_")}_fkey`, columns, referencedSchema: schema, referencedTable, referencedColumns };
}

export function table(name: string, columns: ColumnInfo[], fks: ForeignKeyInfo[] = [], extra: Partial<TableDescription> = {}): TableDescription {
  return {
    schema: "shop",
    name,
    kind: "table",
    comment: null,
    estimatedRows: 100,
    columns: columns.map((c, i) => ({ ...c, ordinal: i + 1 })),
    indexes: [],
    foreignKeys: fks,
    ...extra,
  };
}

export const idCol = () => col("id", "bigint", { isPrimaryKey: true });

/** customers <- orders <- order_items -> products, plus an unrelated audit_log. */
export function shopCatalog(): TableDescription[] {
  return [
    table("customers", [idCol(), col("email", "text"), col("full_name", "text")]),
    table("products", [idCol(), col("sku", "text"), col("price", "numeric(12,2)")]),
    table("orders", [idCol(), col("customer_id"), col("total", "numeric(14,2)")], [fk(["customer_id"], "customers")]),
    table("order_items", [col("order_id", "bigint", { isPrimaryKey: true }), col("product_id", "bigint", { isPrimaryKey: true }), col("qty", "int")], [
      fk(["order_id"], "orders"),
      fk(["product_id"], "products"),
    ]),
    table("audit_log", [idCol(), col("at", "timestamptz")]),
  ];
}
