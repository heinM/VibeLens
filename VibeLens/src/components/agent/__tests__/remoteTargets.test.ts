// Agent UI for the new targets: Redshift suggestions are Redshift-specific (no pg_stat_statements),
// Iceberg still gets the generic list, and tool summaries name Redshift / Iceberg targets without
// assuming a Postgres shape.
import { describe, expect, it } from "vitest";
import { suggestionsFor, type SuggestionContext } from "../suggestions";
import { describeTarget, summarizeToolArgs, targetDetailText } from "../toolSummary";

const ctx = (over: Partial<SuggestionContext> = {}): SuggestionContext => ({ engine: "redshift", connectionName: "wh", database: "dev", access: "results", hasEditorText: false, ...over });

describe("suggestions for Redshift", () => {
  it("are Redshift-shaped: no pg_stat_statements, COMMENT ON or CREATE INDEX advice", () => {
    const s = suggestionsFor(ctx());
    expect(s.length).toBeGreaterThan(2);
    const text = s.map((x) => `${x.title} ${x.prompt}`).join("\n");
    expect(text).toMatch(/dev schema/);
    expect(text).toMatch(/SYS_QUERY_HISTORY/);
    expect(text).not.toMatch(/pg_stat_statements|CREATE INDEX|COMMENT ON/);
  });
  it("gate the query-history suggestion on results access", () => {
    const slow = suggestionsFor(ctx({ access: "schemaOnly" })).find((x) => x.id === "rs-slow");
    expect(slow?.disabledReason).toMatch(/results access/);
    expect(suggestionsFor(ctx()).find((x) => x.id === "rs-slow")?.disabledReason).toBeNull();
  });
  it("offer a review when the editor has SQL, otherwise a query to finish", () => {
    expect(suggestionsFor(ctx({ hasEditorText: true })).some((x) => x.id === "rs-review")).toBe(true);
    const q = suggestionsFor(ctx()).find((x) => x.id === "rs-query");
    expect(q?.incomplete).toBe(true);
  });
  it("never hands Postgres suggestions to an Iceberg or Redshift connection", () => {
    for (const engine of ["redshift", "iceberg"] as const) expect(suggestionsFor(ctx({ engine })).some((x) => x.id.startsWith("pg-"))).toBe(false);
  });
});

describe("approval target detail from Rust (ApprovalTarget.detail)", () => {
  const rs = { connection: "wh", engine: "redshift" as const, environment: "prod" as const, database: null, defaultDatabase: null, valkeyDb: null };
  it("is carried into the card's fixed header as one line of text", () => {
    expect(describeTarget({ ...rs, detail: "Redshift · analytics · dev · eu-west-1 · IAM identity" })).toMatchObject({ connection: "wh", detail: "Redshift · analytics · dev · eu-west-1 · IAM identity" });
    expect(describeTarget({ ...rs, engine: "iceberg", detail: "Trino at trino.example:8443, catalog 'lake', namespace shop, user analyst" })?.detail).toMatch(/^Trino at/);
  });
  it("is absent when Rust sends none (older backends, Postgres, Valkey)", () => {
    expect(describeTarget(rs)).not.toHaveProperty("detail");
    expect(describeTarget({ ...rs, detail: null })).not.toHaveProperty("detail");
    expect(describeTarget({ ...rs, detail: "   " })).not.toHaveProperty("detail");
  });
  it("stays plain text: control characters and line breaks are flattened, length is bounded, markup is not interpreted", () => {
    expect(targetDetailText("a\nb\u0000c d")).toBe("a b c d");
    expect(targetDetailText("x".repeat(500))?.length).toBe(240);
    expect(targetDetailText("<b>**cluster**</b>")).toBe("<b>**cluster**</b>");
  });
});

describe("tool summaries for the new targets", () => {
  it("names a Redshift or Iceberg table without assuming a Postgres argument shape", () => {
    expect(summarizeToolArgs("describe_table", { profile: "wh", schema: "public", table: "orders" })).toBe("public.orders");
    expect(summarizeToolArgs("describe_table", { namespace: ["lake", "sales"], name: "daily" })).toBe("lake.sales.daily");
    expect(summarizeToolArgs("get_schema", { database: "dev" })).toBe("dev");
  });
});
