// Join-condition suggestions for `JOIN x ON |`: foreign keys first (either direction), then a
// conservative naming-convention guess (`orders.customer_id` ~ `customers.id`) when no FK exists.
import type { TableDescription } from "../../ipc/types";
import { type Catalog, ident } from "./catalog";
import { type Relation, exposedName } from "./scope";
import { lookupTable } from "./catalog";

export interface JoinSuggestion {
  /** Text to insert, e.g. `o.customer_id = c.id`. */
  text: string;
  detail: string;
  doc: string;
  fk: boolean;
}

const norm = (s: string) => s.replace(/\s+/g, "").replace(/"/g, "").toLowerCase();

const singular = (s: string): string => (s.endsWith("ies") ? s.slice(0, -3) + "y" : s.endsWith("ses") ? s.slice(0, -2) : s.endsWith("s") ? s.slice(0, -1) : s);

function sameTable(a: TableDescription, schema: string, name: string): boolean {
  return a.schema.toLowerCase() === schema.toLowerCase() && a.name.toLowerCase() === name.toLowerCase();
}

/**
 * `right` is the relation being joined; `lefts` the relations already in the FROM clause. The
 * condition is written left-hand side first: `FROM orders o JOIN customers c ON o.customer_id = c.id`.
 */
export function joinConditions(cat: Catalog | null, right: Relation, lefts: Relation[], existingOnText: string): JoinSuggestion[] {
  const rt = right.kind === "table" ? lookupTable(cat, right.parts) : undefined;
  if (!rt) return [];
  const r = ident(exposedName(right));
  const have = norm(existingOnText);
  const out: JoinSuggestion[] = [];
  const seen = new Set<string>();
  const add = (s: JoinSuggestion) => {
    const key = norm(s.text);
    if (seen.has(key) || (have && have.includes(key))) return;
    seen.add(key);
    out.push(s);
  };

  for (const left of lefts) {
    const lt = left.kind === "table" ? lookupTable(cat, left.parts) : undefined;
    if (!lt) continue;
    const l = ident(exposedName(left));

    // FK on the joined table pointing at the left table: right.fk -> left.pk
    for (const fk of rt.foreignKeys) {
      if (!sameTable(lt, fk.referencedSchema, fk.referencedTable)) continue;
      const text = fk.columns.map((c, i) => `${l}.${ident(fk.referencedColumns[i] ?? "id")} = ${r}.${ident(c)}`).join(" AND ");
      add({ text, detail: `FK ${fk.name}`, doc: `Foreign key \`${rt.name}(${fk.columns.join(", ")})\` references \`${lt.name}(${fk.referencedColumns.join(", ")})\`.`, fk: true });
    }
    // FK on the left table pointing at the joined table: left.fk -> right.pk
    for (const fk of lt.foreignKeys) {
      if (!sameTable(rt, fk.referencedSchema, fk.referencedTable)) continue;
      const text = fk.columns.map((c, i) => `${l}.${ident(c)} = ${r}.${ident(fk.referencedColumns[i] ?? "id")}`).join(" AND ");
      add({ text, detail: `FK ${fk.name}`, doc: `Foreign key \`${lt.name}(${fk.columns.join(", ")})\` references \`${rt.name}(${fk.referencedColumns.join(", ")})\`.`, fk: true });
    }
  }

  // No foreign key anywhere: fall back to `<singular table>_id` naming.
  if (out.length === 0) {
    for (const left of lefts) {
      const lt = left.kind === "table" ? lookupTable(cat, left.parts) : undefined;
      if (!lt) continue;
      const l = ident(exposedName(left));
      const lpk = lt.columns.find((c) => c.isPrimaryKey)?.name ?? "id";
      const rpk = rt.columns.find((c) => c.isPrimaryKey)?.name ?? "id";
      const colFor = (t: TableDescription, other: TableDescription) => t.columns.find((c) => c.name.toLowerCase() === `${singular(other.name.toLowerCase())}_id`);
      const rc = colFor(rt, lt);
      if (rc) add({ text: `${l}.${ident(lpk)} = ${r}.${ident(rc.name)}`, detail: "by naming", doc: `No foreign key found. \`${rt.name}.${rc.name}\` looks like a reference to \`${lt.name}\`.`, fk: false });
      const lc = colFor(lt, rt);
      if (lc) add({ text: `${l}.${ident(lc.name)} = ${r}.${ident(rpk)}`, detail: "by naming", doc: `No foreign key found. \`${lt.name}.${lc.name}\` looks like a reference to \`${rt.name}\`.`, fk: false });
    }
  }
  return out;
}
