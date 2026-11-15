// The two local demo connections offered on the empty sidebar. They match the Docker databases
// that scripts/dev-mac.sh starts (docker-compose.dev.yml): Postgres on 55432, Valkey on 56379.
import type { ConnectionProfile } from "../../ipc/types";
import { newProfile } from "../../lib/defaults";

export interface DemoProfile {
  profile: ConnectionProfile;
  /** Typed password, passed as the second argument of `saveProfile` (it goes straight to the keychain). */
  password?: string;
}

export function demoProfiles(): DemoProfile[] {
  const pg = newProfile("postgres");
  pg.name = "Demo Postgres";
  pg.host = "localhost";
  pg.port = 55432;
  pg.database = "vibelens_test";
  pg.auth = { kind: "password", username: "postgres" };
  pg.tls = { mode: "disable", caCertPath: null, useRdsBundle: false };
  pg.environment = "dev";

  const vk = newProfile("valkey");
  vk.name = "Demo Valkey";
  vk.host = "localhost";
  vk.port = 56379;
  vk.database = null;
  vk.auth = { kind: "none" };
  vk.tls = { mode: "disable", caCertPath: null, useRdsBundle: false };
  vk.environment = "dev";

  return [{ profile: pg, password: "vibelens" }, { profile: vk }];
}
