// Runs a write script through api.pgExecute and waits for its terminal event.
import { api, asError } from "../../ipc/commands";
import type { ErrorPayload } from "../../ipc/types";

export interface StatementOutcome {
  index: number;
  commandTag: string | null;
  rowsAffected: number | null;
}

export type ScriptOutcome =
  | { ok: true; statements: StatementOutcome[]; elapsedMs: number }
  | { ok: false; error: ErrorPayload; statementIndex: number | null };

export function executeScript(args: { profileId: string; database: string | null; sql: string; confirmed: boolean }): Promise<ScriptOutcome> {
  return new Promise((resolve) => {
    const statements: StatementOutcome[] = [];
    let settled = false;
    const settle = (o: ScriptOutcome) => {
      if (settled) return;
      settled = true;
      resolve(o);
    };
    api
      .pgExecute({
        profileId: args.profileId,
        database: args.database,
        sql: args.sql,
        confirmed: args.confirmed,
        options: { readOnly: false },
        onEvent: (ev) => {
          if (ev.type === "statementDone") statements.push({ index: ev.statementIndex, commandTag: ev.commandTag, rowsAffected: ev.rowsAffected });
          else if (ev.type === "finished") settle({ ok: true, statements, elapsedMs: ev.elapsedMs });
          else if (ev.type === "failed") settle({ ok: false, error: ev.error, statementIndex: ev.statementIndex });
        },
      })
      .catch((e) => settle({ ok: false, error: asError(e), statementIndex: null }));
  });
}
