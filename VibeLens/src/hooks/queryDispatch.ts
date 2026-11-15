// Engine dispatch for SQL tabs: which IPC command runs a statement, what cancels it, and which
// cached catalogs a DDL statement makes stale. Pure of React so it is unit-tested with a mocked api.
//
//  * Postgres  -> pgExecute (per database)
//  * Iceberg   -> iceExecute (the connection's catalog; there is no database to choose)
//  * Valkey    -> never runs SQL; it has its own console tab
//
// Both executors take the same channel events and share one running-queries map in the backend,
// so cancelling is always `pgCancel(queryId)`.
import type { QueryClient } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import type { EngineKind, ErrorPayload, ExecOptions, QueryEvent } from "../ipc/types";
import type { StatementStats } from "./useQueryRunner";

export interface ExecArgs {
  profileId: string;
  /** Postgres database; ignored for Iceberg. */
  database: string | null;
  sql: string;
  options?: Partial<ExecOptions>;
  confirmed?: boolean;
  onEvent: (ev: QueryEvent) => void;
}

/** True when SQL tabs can run statements on this engine. */
export const runsSql = (engine: EngineKind | null | undefined): boolean => engine !== "valkey";

/** Starts a run. Resolves with the query id as soon as execution starts; rejects with an ErrorPayload. */
export function executeOnEngine(engine: EngineKind | null | undefined, args: ExecArgs): Promise<string> {
  switch (engine ?? "postgres") {
    case "iceberg":
      return api.iceExecute({ profileId: args.profileId, sql: args.sql, options: args.options, confirmed: args.confirmed, onEvent: args.onEvent });
    case "valkey": {
      const err: ErrorPayload = { code: "unsupported", message: "SQL cannot run on a Valkey connection. Open a Valkey console instead." };
      return Promise.reject(err);
    }
    default:
      return api.pgExecute({ profileId: args.profileId, database: args.database, sql: args.sql, options: args.options, confirmed: args.confirmed, onEvent: args.onEvent });
  }
}

/** Cancels a running statement of either engine (the backend keeps one map for both). */
export function cancelQuery(queryId: string): Promise<void> {
  return api.pgCancel(queryId);
}

/** The `stats` event of a statement, as stored on its result. */
export function statsFromEvent(ev: Extract<QueryEvent, { type: "stats" }>): StatementStats {
  return { engine: ev.engine, bytesScanned: ev.bytesScanned, costUsdEstimate: ev.costUsdEstimate, engineQueryId: ev.engineQueryId };
}

export const DDL_TAG = /^(CREATE|ALTER|DROP|COMMENT|TRUNCATE|REFRESH)\b/i;

/** After DDL: refresh the catalogs completion and the navigator read, for this connection only. */
export function invalidateAfterDdl(qc: QueryClient, engine: EngineKind | null | undefined, profileId: string): void {
  if (engine === "iceberg") {
    // Everything under ["ice", profile]: autocomplete catalog, navigator tree and open table tabs.
    void qc.invalidateQueries({ queryKey: ["ice", profileId] });
    return;
  }
  void qc.invalidateQueries({ predicate: (q) => q.queryKey[0] === "pg" && q.queryKey[1] === profileId && q.queryKey[3] === "schema-metadata" });
}
