import { describe, expect, it } from "vitest";
import { buildInsights } from "../planInsights";
import { parsePlan } from "../planModel";
import { buildExplainPrompt } from "../planPrompt";
import { explainNeedsRedaction, redactConditionText, redactPlanLiterals } from "../planRedact";
import { redactSqlLiterals } from "../../../lib/sqlIntel/redact";

describe("redactConditionText", () => {
  it("replaces string constants and keeps casts", () => {
    expect(redactConditionText("((status)::text = 'pending'::text)")).toBe("((status)::text = ?::text)");
    expect(redactConditionText("(created_at > '2026-01-01 00:00:00+00'::timestamp with time zone)")).toBe("(created_at > ?::timestamp with time zone)");
    expect(redactConditionText("(name = 'O''Brien')")).toBe("(name = ?)");
    expect(redactConditionText("(a = E'x\\'y' AND b = 1)")).toBe("(a = ? AND b = ?)");
    expect(redactConditionText("(pan = ANY ('{4111-1111-1111-1111,5555}'::text[]))")).toBe("(pan = ANY (?::text[]))");
  });
  it("replaces numeric constants", () => {
    expect(redactConditionText("(tenant_id = 42)")).toBe("(tenant_id = ?)");
    expect(redactConditionText("(price > 1.5e3)")).toBe("(price > ?)");
    expect(redactConditionText("(x < -7)")).toBe("(x < -?)");
    expect(redactConditionText("(amount = 0.25)")).toBe("(amount = ?)");
    expect(redactConditionText("(x = ANY (ARRAY[1, 2, 3]))")).toBe("(x = ANY (ARRAY[?, ?, ?]))");
    expect(redactConditionText("((a)::numeric(10,2) > 5.)")).toBe("((a)::numeric(?,?) > ?)");
  });
  it("leaves identifiers, parameters and quoted names alone", () => {
    const same = ["(t2.col1 = t3.id)", `("Order 1"."it's" = $1)`, "(a = $2)", `(x = "col with 'quote")`, "(lower(email) = lower(name))", "(a IS NOT NULL)", "(flag = true)"];
    for (const s of same)
      expect(redactConditionText(s), s).toBe(s);
  });
  it("redacts an unterminated string to the end", () => {
    expect(redactConditionText("(a = 'secret")).toBe("(a = ?");
  });
});

const PLAN = [
  {
    Plan: {
      "Node Type": "Hash Join",
      "Startup Cost": 1,
      "Total Cost": 10,
      "Plan Rows": 5,
      "Hash Cond": "(o.customer_id = c.id)",
      "Join Filter": "(c.email = 'ceo@bank.example'::text)",
      Plans: [
        {
          "Node Type": "Seq Scan",
          "Relation Name": "orders",
          Alias: "o",
          "Startup Cost": 0,
          "Total Cost": 5,
          "Plan Rows": 3,
          Filter: "((status)::text = 'pending'::text)",
          "Rows Removed by Filter": 1234,
          Output: ["o.id", "'literal'::text", "(o.total * 1.2)"],
          "Parent Relationship": "Outer",
        },
        {
          "Node Type": "Index Scan",
          "Relation Name": "customers",
          Alias: "c",
          "Index Name": "customers_pkey",
          "Startup Cost": 0.1,
          "Total Cost": 4,
          "Plan Rows": 1,
          "Index Cond": "(id = 987654)",
          "Recheck Cond": "(id = 987654)",
          "Sort Key": ["c.id", "(c.score + 10)"],
          "Parent Relationship": "Inner",
        },
      ],
    },
  },
];

describe("redactPlanLiterals", () => {
  const out = JSON.stringify(redactPlanLiterals(PLAN));
  it("removes every constant from conditions, outputs and keys", () => {
    for (const leak of ["ceo@bank", "pending", "987654", "literal", "1.2", "+ 10"]) expect(out, leak).not.toContain(leak);
    expect(out).toContain("(o.customer_id = c.id)");
    expect(out).toContain("(c.email = ?::text)");
    expect(out).toContain("(id = ?)");
  });
  it("keeps plan structure and numeric statistics", () => {
    const p = parsePlan(redactPlanLiterals(PLAN));
    expect(p?.nodes).toHaveLength(3);
    expect(p?.nodes[1].props["Rows Removed by Filter"]).toBe(1234);
    expect(p?.nodes[1].relation).toBe("orders");
  });
  it("does not mutate its input and accepts JSON text and the bare object", () => {
    const before = JSON.stringify(PLAN);
    redactPlanLiterals(PLAN);
    expect(JSON.stringify(PLAN)).toBe(before);
    expect(JSON.stringify(redactPlanLiterals(JSON.stringify(PLAN)))).toBe(out);
    expect(JSON.stringify(redactPlanLiterals(PLAN[0]))).toBe(out);
  });
  it("returns null for anything that is not a plan", () => {
    expect(redactPlanLiterals("nope")).toBeNull();
    expect(redactPlanLiterals([])).toBeNull();
    expect(redactPlanLiterals({ a: 1 })).toBeNull();
  });
  it("produces a prompt without any value, SQL included", () => {
    const parsed = parsePlan(redactPlanLiterals(PLAN));
    if (!parsed) throw new Error("plan");
    const prompt = buildExplainPrompt({
      sql: redactSqlLiterals("select * from orders o join customers c on c.id = o.customer_id where c.email = 'ceo@bank.example' and c.id = 987654"),
      plan: parsed,
      insights: buildInsights(parsed),
    });
    for (const leak of ["ceo@bank", "987654", "pending"]) expect(prompt, leak).not.toContain(leak);
    expect(prompt).toContain("c.email = ?");
  });
});

describe("explainNeedsRedaction", () => {
  it("redacts under schemaOnly, none and unknown; under approval only on prod", () => {
    expect(explainNeedsRedaction("schemaOnly")).toBe(true);
    expect(explainNeedsRedaction("none")).toBe(true);
    expect(explainNeedsRedaction(null)).toBe(true);
    expect(explainNeedsRedaction("resultsWithApproval", "prod")).toBe(true);
    expect(explainNeedsRedaction("resultsWithApproval", "dev")).toBe(false);
    expect(explainNeedsRedaction("results", "prod")).toBe(false);
  });
});
