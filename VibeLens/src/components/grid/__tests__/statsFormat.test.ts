import { describe, expect, it } from "vitest";
import { COST_FLOOR_USD, MIN_BILLED_BYTES, costTooltip, engineBadge, formatCostUsd, humanBytes, scannedTooltip, shortQueryId, statsView } from "../statsFormat";

describe("humanBytes", () => {
  it("uses binary units with three significant digits", () => {
    expect(humanBytes(0)).toBe("0 B");
    expect(humanBytes(512)).toBe("512 B");
    expect(humanBytes(1536)).toBe("1.50 KB");
    expect(humanBytes(224393)).toBe("219 KB");
    expect(humanBytes(12.34 * 1024 * 1024)).toBe("12.3 MB");
    expect(humanBytes(3.2 * 1024 ** 3)).toBe("3.20 GB");
    expect(humanBytes(5 * 1024 ** 4)).toBe("5.00 TB");
  });
  it("never prints 1024 of a unit", () => {
    expect(humanBytes(1024 * 1024 - 1)).toBe("1.00 MB");
    expect(humanBytes(1024 ** 3 - 10)).toBe("1.00 GB");
  });
  it("copes with junk", () => {
    expect(humanBytes(-5)).toBe("0 B");
    expect(humanBytes(NaN)).toBe("0 B");
  });
});

describe("formatCostUsd", () => {
  it("rounds to four decimals under a dollar", () => {
    expect(formatCostUsd(0.0123)).toBe("≈ $0.0123");
    expect(formatCostUsd(0.01234567)).toBe("≈ $0.0123");
    expect(formatCostUsd(0.5)).toBe("≈ $0.5000");
  });
  it("shows a bound instead of $0.0000 for the 10 MB minimum", () => {
    // 10 MiB at $5/TiB is about $0.0000477
    expect(formatCostUsd((10 * 1024 * 1024) / 1_099_511_627_776 * 5)).toBe("< $0.0001");
    expect(formatCostUsd(0)).toBe("no charge");
    expect(formatCostUsd(COST_FLOOR_USD)).toBe("≈ $0.0001");
  });
  it("uses two decimals and separators from a dollar up", () => {
    expect(formatCostUsd(1)).toBe("≈ $1.00");
    expect(formatCostUsd(12.345)).toBe("≈ $12.35");
    expect(formatCostUsd(1234.5)).toBe("≈ $1,234.50");
    // 0.99996 would round up to 1.0000 with four decimals
    expect(formatCostUsd(0.99996)).toBe("≈ $1.00");
  });
  it("has nothing to say without an estimate", () => {
    expect(formatCostUsd(null)).toBeNull();
    expect(formatCostUsd(undefined)).toBeNull();
    expect(formatCostUsd(NaN)).toBeNull();
    expect(formatCostUsd(-1)).toBeNull();
  });
});

describe("tooltips", () => {
  it("calls the cost an estimate and names the assumptions", () => {
    const t = costTooltip(2 * 1024 ** 3);
    expect(t).toMatch(/estimate/i);
    expect(t).toContain("$5 per TB");
    expect(t).toContain("10 MB minimum");
    expect(t).toMatch(/region/);
    expect(t).toMatch(/discounts/);
    expect(t).toContain("2.00 GB scanned");
  });
  it("explains the minimum on a small scan", () => {
    expect(costTooltip(2048)).toContain("scanned 2.00 KB, so the 10 MB minimum applies");
    expect(costTooltip(MIN_BILLED_BYTES - 1)).toContain("minimum applies");
    expect(costTooltip(MIN_BILLED_BYTES)).toContain("Based on 10.0 MB scanned");
    expect(costTooltip(null)).not.toMatch(/Based on|This query scanned/);
  });
  it("says whose bytes they are", () => {
    expect(scannedTooltip("athena", 1536)).toContain("1,536 bytes");
    expect(scannedTooltip("athena", 1536)).toContain("bills by data scanned");
    expect(scannedTooltip("datafusion", 1536)).toContain("read from storage");
  });
});

describe("engine badge", () => {
  it("names the two engines", () => {
    expect(engineBadge("datafusion").label).toBe("DataFusion · local");
    expect(engineBadge("DataFusion").label).toBe("DataFusion · local");
    expect(engineBadge("athena").label).toBe("Athena");
    expect(engineBadge("athena").tone).toBe("accent");
    expect(engineBadge("trino").label).toBe("trino");
  });
});

describe("statsView", () => {
  it("combines everything for an Athena statement", () => {
    const v = statsView({ engine: "athena", bytesScanned: 3_221_225_472, costUsdEstimate: 0.01467, engineQueryId: "5f1c2e9a-0b3d-4c7e-9a41-77d3c0de1234" });
    expect(v.engine.label).toBe("Athena");
    expect(v.scanned?.text).toBe("3.00 GB scanned");
    expect(v.cost?.text).toBe("≈ $0.0147");
    expect(v.cost?.title).toMatch(/estimate/i);
    expect(v.queryId).toEqual({ short: "5f1c2e9a…", full: "5f1c2e9a-0b3d-4c7e-9a41-77d3c0de1234" });
  });
  it("shows only what the local engine knows", () => {
    const v = statsView({ engine: "datafusion", bytesScanned: 224393, costUsdEstimate: null, engineQueryId: null });
    expect(v.engine.label).toBe("DataFusion · local");
    expect(v.scanned?.text).toBe("219 KB scanned");
    expect(v.cost).toBeNull();
    expect(v.queryId).toBeNull();
  });
  it("copes with an engine that reports nothing but its name", () => {
    const v = statsView({ engine: "athena", bytesScanned: null, costUsdEstimate: null, engineQueryId: null });
    expect(v.scanned).toBeNull();
    expect(v.cost).toBeNull();
  });
  it("keeps a zero scan (a metadata-only query)", () => {
    expect(statsView({ engine: "athena", bytesScanned: 0, costUsdEstimate: 4.77e-5, engineQueryId: null }).scanned?.text).toBe("0 B scanned");
  });
});

describe("shortQueryId", () => {
  it("keeps the front of long ids", () => {
    expect(shortQueryId("5f1c2e9a-0b3d-4c7e")).toBe("5f1c2e9a…");
    expect(shortQueryId("abc")).toBe("abc");
    expect(shortQueryId("123456789")).toBe("123456789");
  });
});
