export type SqlTokenKind = "kw" | "str" | "num" | "com" | "text";
export interface SqlToken {
  k: SqlTokenKind;
  v: string;
}

const KEYWORDS = new Set(
  (
    "select from where group by order having limit offset insert into values update set delete create alter drop table index view materialized " +
    "schema database sequence extension function procedure trigger if not exists and or in is null as join left right inner outer full cross on using union all " +
    "distinct case when then else end begin commit rollback savepoint with returning primary key foreign references unique check default constraint add column " +
    "cascade restrict truncate grant revoke explain analyze vacuum cluster reindex copy do return returns language true false asc desc like ilike between " +
    "any some over partition window row rows only for concurrently temp temporary unlogged lateral"
  ).split(" "),
);

/** A forgiving, single-pass tokenizer for display purposes only. */
export function tokenizeSql(sql: string): SqlToken[] {
  const out: SqlToken[] = [];
  const push = (k: SqlTokenKind, v: string) => {
    const last = out[out.length - 1];
    if (last && last.k === k && k === "text") last.v += v;
    else out.push({ k, v });
  };
  let i = 0;
  while (i < sql.length) {
    const rest = sql.slice(i);
    let m: RegExpExecArray | null;
    if ((m = /^--[^\n]*/.exec(rest)) || (m = /^\/\*[\s\S]*?(\*\/|$)/.exec(rest))) push("com", m[0]);
    else if ((m = /^'(?:[^']|'')*('|$)/.exec(rest))) push("str", m[0]);
    else if ((m = /^\$([A-Za-z_]\w*)?\$[\s\S]*?(\$\1\$|$)/.exec(rest)) && m[0].length > 2) push("str", m[0]);
    else if ((m = /^"(?:[^"]|"")*("|$)/.exec(rest))) push("text", m[0]);
    else if ((m = /^\d+(\.\d+)?\b/.exec(rest))) push("num", m[0]);
    else if ((m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(rest))) push(KEYWORDS.has(m[0].toLowerCase()) ? "kw" : "text", m[0]);
    else {
      m = /^[\s\S]/.exec(rest);
      push("text", m ? m[0] : rest[0]);
    }
    i += m ? m[0].length : 1;
  }
  return out;
}
