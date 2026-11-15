import { describe, expect, it } from "vitest";
import { generateInsert, generateSelect, generateUpdate, quoteIdent, sampleValue, selectStar } from "../sqlTemplates";
import { demoProfiles } from "../demoProfiles";
import { col, idCol, table } from "../../erd/fixtures";

const orders = table("orders", [
  { ...idCol(), default: "nextval('shop.orders_id_seq'::regclass)" },
  col("customer_id"),
  col("status", "shop.order_status", { default: "'pending'::shop.order_status" }),
  col("total", "numeric(14,2)", { default: "0" }),
  col("notes", "text", { nullable: true }),
  col("placed_at", "timestamptz"),
]);

describe("sql templates", () => {
  it("quotes identifiers, escaping embedded quotes", () => {
    expect(quoteIdent('we"ird')).toBe('"we""ird"');
    expect(selectStar("My Schema", "t")).toBe('SELECT * FROM "My Schema"."t" LIMIT 100;');
  });

  it("SELECT lists every column in order and limits rows", () => {
    expect(generateSelect(orders)).toBe(
      'SELECT\n    "id",\n    "customer_id",\n    "status",\n    "total",\n    "notes",\n    "placed_at"\nFROM "shop"."orders"\nLIMIT 100;\n',
    );
  });

  it("SELECT falls back to * for a table without columns", () => {
    expect(generateSelect(table("empty", []))).toContain("    *\nFROM");
  });

  it("INSERT skips generated keys, uses DEFAULT/NULL/typed samples and comments the types", () => {
    const sql = generateInsert(orders);
    expect(sql).not.toContain('"id"');
    expect(sql).toContain('INSERT INTO "shop"."orders" (\n    "customer_id",\n    "status",\n    "total",\n    "notes",\n    "placed_at"\n)');
    expect(sql).toContain("    0,  -- customer_id bigint not null");
    expect(sql).toContain("    DEFAULT,  -- status shop.order_status not null");
    expect(sql).toContain("    NULL,  -- notes text");
    expect(sql).toContain("    now()  -- placed_at timestamptz not null\n);");
  });

  it("INSERT on a table of only generated columns uses DEFAULT VALUES", () => {
    expect(generateInsert(table("ids", [{ ...idCol(), default: "nextval('s')" }]))).toBe('INSERT INTO "shop"."ids" DEFAULT VALUES;\n');
  });

  it("UPDATE sets non-key columns and guards on the primary key", () => {
    const sql = generateUpdate(orders);
    expect(sql).toContain('UPDATE "shop"."orders"\nSET\n    "customer_id" = 0,  -- bigint');
    expect(sql).not.toContain('"id" = 0,');
    expect(sql).toMatch(/WHERE "id" = 0;\n$/);
  });

  it("UPDATE on a keyless table can never match every row by accident", () => {
    const sql = generateUpdate(table("log", [col("msg", "text")]));
    expect(sql).toContain("WHERE false");
  });

  it("composite keys are ANDed", () => {
    const t = table("oi", [col("a", "bigint", { isPrimaryKey: true }), col("b", "bigint", { isPrimaryKey: true }), col("q", "int")]);
    expect(generateUpdate(t)).toContain('WHERE "a" = 0\n  AND "b" = 0;');
  });

  it("sampleValue covers common types", () => {
    expect(sampleValue("integer")).toBe("0");
    expect(sampleValue("boolean")).toBe("false");
    expect(sampleValue("uuid")).toBe("gen_random_uuid()");
    expect(sampleValue("timestamp with time zone")).toBe("now()");
    expect(sampleValue("jsonb")).toBe("'{}'");
    expect(sampleValue("text[]")).toBe("'{}'");
    expect(sampleValue("character varying")).toBe("''");
  });
});

describe("demoProfiles", () => {
  it("match the docker-compose dev databases", () => {
    const [pg, vk] = demoProfiles();
    expect(pg.profile).toMatchObject({ name: "Demo Postgres", engine: "postgres", host: "localhost", port: 55432, database: "vibelens_test", environment: "dev" });
    expect(pg.profile.auth).toEqual({ kind: "password", username: "postgres" });
    expect(pg.profile.tls.mode).toBe("disable");
    expect(pg.password).toBe("vibelens");
    expect(vk.profile).toMatchObject({ name: "Demo Valkey", engine: "valkey", port: 56379 });
    expect(vk.profile.auth).toEqual({ kind: "none" });
    expect(vk.profile.tls.mode).toBe("disable");
    expect(vk.password).toBeUndefined();
  });
});
