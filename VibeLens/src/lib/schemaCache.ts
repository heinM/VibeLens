// Shared, cached schema catalog for a (connection, database). Used by editor autocomplete,
// the command palette and ER diagrams so the catalog is fetched once, not per feature.
//
// Two engines feed the same `SchemaMetadata` shape: Postgres (`pg_schema_metadata`, per database)
// and Iceberg (`ice_schema_metadata`, whole catalog, schema = dotted namespace). Their cache keys
// start with different roots (`pg` / `ice`) so the two can never be served for one another.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import type { SchemaMetadata, TableDescription } from "../ipc/types";

/** Engines that expose a browsable schema catalog. */
export type CatalogEngine = "postgres" | "iceberg";

/** Query key of the catalog. `database` is ignored for Iceberg (the catalog has no databases). */
export const schemaMetadataKey = (profileId: string | null, database: string | null, engine: CatalogEngine = "postgres") =>
  engine === "iceberg" ? (["ice", profileId, "schema-metadata"] as const) : (["pg", profileId, database, "schema-metadata"] as const);

/** True for a query key produced by `schemaMetadataKey` (either engine) for this profile. */
export const isSchemaMetadataKeyOf = (key: readonly unknown[], profileId: string): boolean =>
  (key[0] === "pg" && key[1] === profileId && key[3] === "schema-metadata") || (key[0] === "ice" && key[1] === profileId && key[2] === "schema-metadata");

/** Fetches the catalog from the engine's own command. */
export function fetchSchemaMetadata(engine: CatalogEngine, profileId: string, database: string | null): Promise<SchemaMetadata> {
  return engine === "iceberg" ? api.iceSchemaMetadata(profileId, null) : api.pgSchemaMetadata(profileId, database);
}

const STALE_MS = 5 * 60_000;

/** Whole-database catalog (all non-system schemas). Disabled until a profile id is given. */
export function useSchemaMetadata(profileId: string | null, database: string | null, enabled = true, engine: CatalogEngine = "postgres") {
  return useQuery({
    queryKey: schemaMetadataKey(profileId, database, engine),
    queryFn: () => fetchSchemaMetadata(engine, profileId as string, database),
    enabled: enabled && !!profileId,
    staleTime: STALE_MS,
  });
}

/** Imperative access (e.g. inside a Monaco completion provider). Fetches if not cached. */
export function useSchemaMetadataLoader() {
  const qc = useQueryClient();
  return (profileId: string, database: string | null, engine: CatalogEngine = "postgres"): Promise<SchemaMetadata> =>
    qc.fetchQuery({ queryKey: schemaMetadataKey(profileId, database, engine), queryFn: () => fetchSchemaMetadata(engine, profileId, database), staleTime: STALE_MS });
}

/** Look up a table by schema-qualified or bare name (bare names match search_path-ish: public first). */
export function findTable(meta: SchemaMetadata | undefined, name: string, schema?: string | null): TableDescription | undefined {
  if (!meta) return undefined;
  const n = name.toLowerCase();
  if (schema) return meta.tables.find((t) => t.schema.toLowerCase() === schema.toLowerCase() && t.name.toLowerCase() === n);
  return meta.tables.find((t) => t.schema === "public" && t.name.toLowerCase() === n) ?? meta.tables.find((t) => t.name.toLowerCase() === n);
}
