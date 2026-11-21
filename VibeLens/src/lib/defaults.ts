import type { ConnectionProfile, EngineKind, IcebergOptions, RedshiftOptions } from "../ipc/types";

/** Defaults for a new Iceberg connection: a local REST catalog (matches docker-compose.dev.yml). */
export function newIcebergOptions(): IcebergOptions {
  return {
    catalog: { kind: "rest", uri: "http://localhost:8181", warehouse: null, auth: { kind: "none" }, headers: {} },
    storage: { kind: "auto" },
    queryEngine: { kind: "local" },
    defaultNamespace: null,
  };
}

/** Defaults for a new Redshift connection: a provisioned cluster, signed in as the caller's IAM identity. */
export function newRedshiftOptions(region = ""): RedshiftOptions {
  return {
    target: { kind: "provisioned", clusterIdentifier: "" },
    database: "dev",
    aws: { identity: { kind: "profile", profileName: "" }, region },
    auth: { kind: "iamIdentity" },
  };
}

/** Engines reached at host:port. Iceberg and Redshift go through catalogs and AWS APIs instead. */
export const usesHostPort = (engine: EngineKind): boolean => engine === "postgres" || engine === "valkey";

/** A blank profile with the same defaults as the Rust `Default` impls. */
export function newProfile(engine: EngineKind): ConnectionProfile {
  return {
    id: "",
    name: "",
    engine,
    host: usesHostPort(engine) ? "localhost" : "",
    port: engine === "postgres" ? 5432 : engine === "valkey" ? 6379 : 0,
    database: engine === "postgres" ? "postgres" : null,
    auth: engine === "postgres" ? { kind: "password", username: "postgres" } : { kind: "none" },
    tls: { mode: engine === "postgres" ? "verifyFull" : "disable", caCertPath: null, useRdsBundle: false },
    tunnel: null,
    environment: "dev",
    aiDataAccess: "schemaOnly",
    color: null,
    pg: { connectTimeoutSecs: 30, statementTimeoutMs: null, idleDisconnectSecs: 300, maxPoolSize: 4, applicationName: "VibeLens", pgproxy: null },
    valkey: { cluster: false, keyDelimiter: ":", scanCount: 500, connectTimeoutSecs: 15, idleDisconnectSecs: 300 },
    createdAt: null,
    updatedAt: null,
    iceberg: engine === "iceberg" ? newIcebergOptions() : null,
    redshift: engine === "redshift" ? newRedshiftOptions() : null,
  };
}
