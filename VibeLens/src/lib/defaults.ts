import type { ConnectionProfile, EngineKind, IcebergOptions, MysqlOptions, RedshiftOptions } from "../ipc/types";

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

/** Same values as the Rust `MysqlOptions::default()`. */
export function newMysqlOptions(): MysqlOptions {
  return { connectTimeoutSecs: 30, statementTimeoutMs: null, idleDisconnectSecs: 300, maxPoolSize: 4 };
}

/** Engines reached at host:port. Iceberg and Redshift go through catalogs and AWS APIs instead. */
export const usesHostPort = (engine: EngineKind): boolean => engine === "postgres" || engine === "valkey" || engine === "mysql";

/** Default TCP port of an engine reached at host:port (0 when it has none). */
export const defaultPort = (engine: EngineKind): number =>
  engine === "postgres" ? 5432 : engine === "valkey" ? 6379 : engine === "mysql" ? 3306 : 0;

/** A blank profile with the same defaults as the Rust `Default` impls. */
export function newProfile(engine: EngineKind): ConnectionProfile {
  return {
    id: "",
    name: "",
    engine,
    host: usesHostPort(engine) ? "localhost" : "",
    port: defaultPort(engine),
    database: engine === "postgres" ? "postgres" : engine === "mysql" ? "" : null,
    auth:
      engine === "postgres"
        ? { kind: "password", username: "postgres" }
        : engine === "mysql"
          ? { kind: "password", username: "root" }
          : { kind: "none" },
    tls: { mode: engine === "postgres" || engine === "mysql" ? "verifyFull" : "disable", caCertPath: null, useRdsBundle: false },
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
    mysql: newMysqlOptions(),
  };
}
