// Display names of the connection engines. Typed as full records so a new EngineKind fails to compile
// until every list, tooltip and badge names it.
import type { EngineKind } from "../ipc/types";

/** Product name, for titles and tooltips. */
export const ENGINE_NAME: Record<EngineKind, string> = {
  postgres: "PostgreSQL",
  valkey: "Valkey",
  iceberg: "Apache Iceberg",
  redshift: "Amazon Redshift",
};

/** Short name, for badges and compact rows. */
export const ENGINE_SHORT: Record<EngineKind, string> = {
  postgres: "Postgres",
  valkey: "Valkey",
  iceberg: "Iceberg",
  redshift: "Redshift",
};

/** Engines whose connections run SQL in worksheets (Valkey has its own console). */
export const isSqlEngine = (engine: EngineKind | null | undefined): engine is "postgres" | "iceberg" | "redshift" =>
  engine === "postgres" || engine === "iceberg" || engine === "redshift";
