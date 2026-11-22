// MySQL connection form rules. They mirror crates/vibelens-core/src/mysql/validate.rs and
// mysql/secret.rs; Rust applies the same rules on save and on every connect, so these are a
// convenience that says what is wrong before the round trip.
import type { AuthMethod, ConnectionProfile, MysqlOptions } from "../../ipc/types";
import { newMysqlOptions } from "../../lib/defaults";
import { isLoopbackHost } from "./remoteEngineForm";
import type { FieldErrors } from "./validation";

/** Sign-in methods a MySQL connection accepts. */
export const MYSQL_AUTH_KINDS: AuthMethod["kind"][] = ["password", "rdsIam"];

const intIn = (v: number, min: number, max: number) => Number.isInteger(v) && v >= min && v <= max;
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

/** A MySQL database name as Rust accepts it: 1-64 characters, no control characters, no outer spaces. */
export function mysqlDatabaseError(name: string | null | undefined): string | null {
  const v = name ?? "";
  if (!v.trim()) return "A default database is required for MySQL connections.";
  if ([...v].length > 64) return "At most 64 characters.";
  if (CONTROL.test(v)) return "Remove control characters.";
  if (v !== v.trim()) return "Remove the spaces at the start or end.";
  return null;
}

/** The host is this machine and no tunnel is used: the only case where TLS may be skipped for a password. */
export function mysqlIsLocal(p: ConnectionProfile): boolean {
  return !p.tunnel && isLoopbackHost(p.host.trim());
}

/** Rust's `mysql::validate::check_profile`, field by field. Keys use the wizard's section prefixes. */
export function validateMysql(p: ConnectionProfile): FieldErrors {
  const e: FieldErrors = {};
  const db = mysqlDatabaseError(p.database);
  if (db) e.database = db;
  const a = p.auth;
  if (a.kind === "password" || a.kind === "rdsIam") {
    const u = a.username.trim();
    if (!u) e["auth.username"] = "User name is required.";
    else if ([...u].length > 32) e["auth.username"] = "MySQL user names are at most 32 characters.";
    else if (CONTROL.test(u)) e["auth.username"] = "Remove control characters.";
  } else {
    e["auth.kind"] = "MySQL connections use a password or RDS IAM.";
  }
  if (a.kind === "rdsIam" && p.tls.mode !== "verifyFull")
    e["tls.mode"] = "RDS IAM sends its token with the cleartext plugin, so it needs Verify full (turn on the Amazon RDS CA bundle).";
  if (a.kind === "password" && !mysqlIsLocal(p) && p.tls.mode !== "verifyFull")
    e["tls.mode"] = p.tunnel
      ? "A password sent through an SSH tunnel needs Verify full: the hop from the bastion to the database is not protected by the tunnel."
      : "A password sent to another machine needs Verify full: without verification, whoever is in the path can obtain a reusable credential.";
  const o: MysqlOptions = p.mysql ?? newMysqlOptions();
  if (!intIn(o.connectTimeoutSecs, 1, 600)) e["my.connectTimeoutSecs"] = "1-600 seconds.";
  if (o.statementTimeoutMs != null && !intIn(o.statementTimeoutMs, 1, 86_400_000)) e["my.statementTimeoutMs"] = "Positive milliseconds, or leave empty for none.";
  if (!intIn(o.idleDisconnectSecs, 0, 86_400)) e["my.idleDisconnectSecs"] = "0-86400 seconds.";
  if (!intIn(o.maxPoolSize, 1, 32)) e["my.maxPoolSize"] = "1-32 connections.";
  return e;
}

/**
 * What a saved MySQL password is bound to (Rust's `mysql::secret::password_origin`, minus the CA file's
 * contents, which the UI cannot read): host, port, user, TLS mode, CA file path, RDS bundle switch and
 * SSH tunnel endpoint. Null when the profile has no stored password.
 */
export function mysqlPasswordOrigin(p: ConnectionProfile): string | null {
  if (p.engine !== "mysql" || p.auth.kind !== "password") return null;
  const t = p.tunnel ? `ssh://${p.tunnel.username.trim()}@${p.tunnel.host.trim().toLowerCase()}:${p.tunnel.port}` : "";
  return [p.host.trim().toLowerCase(), p.port, p.auth.username.trim(), p.tls.mode, p.tls.caCertPath?.trim() ?? "", p.tls.useRdsBundle, t].join("|");
}

/**
 * True when saving this edit without a new password would make Rust delete the saved one: the password
 * is used only for the endpoint it was saved for.
 */
export function mysqlPasswordUnbound(original: ConnectionProfile | null, p: ConnectionProfile): boolean {
  if (!original || original.engine !== "mysql") return false;
  const before = mysqlPasswordOrigin(original);
  const after = mysqlPasswordOrigin(p);
  return before != null && after != null && before !== after;
}

/**
 * Password errors that depend on the draft and the keychain: a MySQL password is required (Rust refuses to
 * connect without one), and must be typed again when the endpoint it was saved for has changed.
 */
export function mysqlPasswordError(original: ConnectionProfile | null, p: ConnectionProfile, edit: { mode: "keep" | "set" | "remove"; value: string }, hasStored: boolean): string | null {
  if (p.engine !== "mysql" || p.auth.kind !== "password") return null;
  if (edit.mode === "set" && edit.value) return null;
  const stored = hasStored && edit.mode === "keep" && original?.engine === "mysql" && original.auth.kind === "password";
  if (stored && !mysqlPasswordUnbound(original, p)) return null;
  if (stored) return "The saved password is tied to the old host, port, user, TLS or tunnel settings and will be deleted. Enter it again.";
  return "Enter the password.";
}
