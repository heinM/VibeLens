// Pure redaction helpers. Three different jobs, one place:
//
//  * `redactDbError`   : what the "Fix with AI" prompt may say about a server error (row values out).
//  * `redactForHistory`: secrets typed into SQL (passwords, encryption keys, cloud credentials) out of
//                        what the query history stores on disk.
//  * `redactSqlLiterals` : every string / numeric constant out of a statement (Explain with AI under a
//                        schema-only policy).
//
// None of this is a parser. It errs on the side of removing too much, never on the side of keeping a value.
import type { AiDataAccess, Environment } from "../../ipc/types";
import { type Token, tokenize } from "./tokenizer";

export const REDACTED = "‹redacted›";

// ---------------------------------------------------------------------------------------------
// Server errors
// ---------------------------------------------------------------------------------------------

/** Words after which a double-quoted name in a server message is an identifier (schema), not a value. */
const NAME_NOUN = /\b(?:column|columns|relation|table|constraint|index|schema|function|type|sequence|view|trigger|role|user|database|operator|extension|language|policy|collation|alias|aggregate|domain|procedure|tablespace|publication|subscription|near)\s*$/i;

/**
 * Replaces quoted spans in a server message. `'...'` is always a literal. `"..."` is kept only when
 * it names a schema object (`column "ssn"`, `at or near "FROM"`); anywhere else (`type integer:
 * "secret"`, `value "x" is out of range`) it is a data value. A double quote that follows `: `
 * is a value that Postgres does not escape, so the span runs to the last `"` on that line.
 */
function redactQuoted(text: string): string {
  let out = "";
  let i = 0;
  const n = text.length;
  while (i < n) {
    const ch = text[i];
    if (ch === "'") {
      let j = i + 1;
      while (j < n) {
        if (text[j] === "'") {
          if (text[j + 1] === "'") {
            j += 2;
            continue;
          }
          break;
        }
        j++;
      }
      out += REDACTED;
      i = Math.min(j + 1, n);
    } else if (ch === '"') {
      const afterColon = /:\s*$/.test(out);
      let j: number;
      if (afterColon) {
        const eol = text.indexOf("\n", i);
        const lineEnd = eol < 0 ? n : eol;
        const last = text.lastIndexOf('"', lineEnd - 1);
        j = last > i ? last : lineEnd - 1;
      } else {
        j = i + 1;
        while (j < n) {
          if (text[j] === '"') {
            if (text[j + 1] === '"') {
              j += 2;
              continue;
            }
            break;
          }
          j++;
        }
      }
      const closed = j < n && text[j] === '"';
      if (!afterColon && closed && NAME_NOUN.test(out)) out += text.slice(i, j + 1);
      else out += REDACTED;
      i = Math.min(j + 1, n);
    } else {
      out += ch;
      i++;
    }
  }
  return out;
}

const KEY_VALUE_GROUP = /\([^()\n]*\)=\([^\n]*\)/g;
const SQLSTATE_RE = /\bSQLSTATE[\s:=\[]*([0-9A-Z]{5})\b/;
const DROPPED_LINE = /^\s*(?:DETAIL|WHERE|CONTEXT|QUERY|STATEMENT|LOCATION|SCHEMA NAME|TABLE NAME|COLUMN NAME|DATA TYPE NAME|CONSTRAINT NAME)\s*:/i;

/**
 * The text of a server error that may be sent to the model. Under `none` and `schemaOnly` (and
 * `resultsWithApproval` on prod, or when the access level is unknown) row data must not leave: the
 * result is an optional SQLSTATE, then the primary message with quoted literals and `(col)=(value)`
 * groups replaced by ‹redacted›; DETAIL / WHERE / CONTEXT lines are dropped. Under `results` (and
 * `resultsWithApproval` off prod) the message is passed through unchanged.
 */
export function redactDbError(message: string, access: AiDataAccess | null | undefined, environment?: Environment | null): string {
  const passThrough = access === "results" || (access === "resultsWithApproval" && environment !== "prod");
  if (passThrough) return message;
  const sqlstate = SQLSTATE_RE.exec(message)?.[1] ?? null;
  // Quotes first (a quoted value can span lines and contain "\nDETAIL:"), then drop the extra lines.
  const text = redactQuoted(message.replace(/\r\n?/g, "\n"));
  const lines = text.split("\n");
  const kept: string[] = [];
  for (const [idx, line] of lines.entries()) {
    if (idx > 0 && DROPPED_LINE.test(line)) continue;
    kept.push(line.replace(KEY_VALUE_GROUP, REDACTED));
  }
  // The primary message is the first line; a HINT line is generic advice and stays.
  const primary = (kept[0] ?? "").replace(SQLSTATE_RE, "").replace(/^[\s:,-]+/, "").trim();
  const hints = kept.slice(1).filter((l) => /^\s*HINT\s*:/i.test(l));
  const body = [primary, ...hints].filter((l) => l.trim() !== "").join("\n");
  return sqlstate ? `SQLSTATE ${sqlstate}: ${body}`.trim() : body;
}

/** Drops DETAIL / WHERE / CONTEXT / QUERY lines (row data lives there) from a stored error message. */
export function stripErrorDetail(message: string): string {
  const lines = message.replace(/\r\n?/g, "\n").split("\n");
  return lines.filter((l, i) => i === 0 || !DROPPED_LINE.test(l)).join("\n");
}

// ---------------------------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------------------------

interface Edit {
  start: number;
  end: number;
  text: string;
}

function applyEdits(sql: string, edits: Edit[]): string {
  if (edits.length === 0) return sql;
  const sorted = [...edits].sort((a, b) => a.start - b.start);
  let out = "";
  let pos = 0;
  for (const e of sorted) {
    if (e.start < pos) continue; // overlapping edit: the earlier one already covers it
    out += sql.slice(pos, e.start) + e.text;
    pos = e.end;
  }
  return out + sql.slice(pos);
}

const STRING_REDACTED = `'${REDACTED}'`;

/**
 * Functions whose argument at the given 0-based position is a secret (a key or password).
 * pgcrypto: pgp_sym_*(data, KEY[, opts]), pgp_pub_decrypt(data, privkey, PASSPHRASE), encrypt/decrypt(data, KEY, type),
 * crypt(PASSWORD, salt); aws_commons.create_aws_credentials(id, SECRET, token).
 */
const SECRET_ARGS: Record<string, number[]> = {
  pgp_sym_encrypt: [1],
  pgp_sym_decrypt: [1],
  pgp_sym_encrypt_bytea: [1],
  pgp_sym_decrypt_bytea: [1],
  pgp_pub_decrypt: [2],
  pgp_pub_decrypt_bytea: [2],
  encrypt: [1],
  decrypt: [1],
  encrypt_iv: [1],
  decrypt_iv: [1],
  crypt: [0],
  create_aws_credentials: [1, 2],
};

/** String-literal tokens of one function-call argument list. */
function secretArgEdits(toks: Token[], open: number, positions: number[]): Edit[] {
  const edits: Edit[] = [];
  let depth = 0;
  let arg = 0;
  for (let k = open; k < toks.length; k++) {
    const t = toks[k];
    if (t.kind === "punct") {
      if (t.text === "(" || t.text === "[") depth++;
      else if (t.text === ")" || t.text === "]") {
        depth--;
        if (depth === 0) break;
      } else if (t.text === "," && depth === 1) arg++;
      continue;
    }
    if (depth >= 1 && positions.includes(arg) && t.kind === "string") edits.push({ start: t.start, end: t.end, text: STRING_REDACTED });
  }
  return edits;
}

// `password = value` / `password: value` inside conninfo strings and options; value quoted either
// the SQL way ('..'), the doubled-inside-a-literal way (''..''), or bare.
const VALUE = String.raw`(''(?:[^']|'(?!'))+''|'(?:[^']|'')*'|"(?:[^"]|"")*"|[^\s;,)'"&]+)`;
const SECRET_NAME = String.raw`(?:[A-Za-z0-9_.]*?(?:password|passwd|pwd|passfile|secret_access_key|secret_key|session_token|security_token|access_token|auth_token|api_key|apikey|client_secret|private_key|sslpassword))`;
const NAME_VALUE = new RegExp(String.raw`(\b${SECRET_NAME})(\s*(?:=|:(?!:))\s*)${VALUE}`, "gi");
const URI_CREDENTIALS = /\b([a-z][a-z0-9+.-]*:\/\/[^\s:/@'"]+):([^\s@'"/]+)@/gi;
const AWS_KEY_ID = /\b(?:AKIA|ASIA|AGPA|AIDA|AROA)[0-9A-Z]{16}\b/g;

function maskValue(quoted: string): string {
  if (quoted.startsWith("''")) return `''${REDACTED}''`;
  if (quoted.startsWith("'")) return `'${REDACTED}'`;
  if (quoted.startsWith('"')) return `"${REDACTED}"`;
  return REDACTED;
}

/**
 * SQL as it may be written to the history: passwords in CREATE/ALTER ROLE|USER (and `OPTIONS (password '…')`),
 * `password=…` in connection strings (dblink, postgres_fdw, URIs with credentials), encryption keys
 * passed to pgcrypto functions, and AWS-style secrets are replaced by ‹redacted›. Everything else is
 * left exactly as typed.
 */
export function redactForHistory(sql: string): string {
  let toks: Token[];
  try {
    toks = tokenize(sql);
  } catch {
    toks = [];
  }
  const edits: Edit[] = [];
  for (let i = 0; i < toks.length; i++) {
    const t = toks[i];
    if (t.kind !== "word") continue;
    const w = t.text.toLowerCase();
    // PASSWORD '…' (also ENCRYPTED PASSWORD, WITH PASSWORD, OPTIONS (password '…')).
    if (w === "password" || w === "passwd") {
      const nx = toks[i + 1];
      if (nx?.kind === "string") edits.push({ start: nx.start, end: nx.end, text: STRING_REDACTED });
      continue;
    }
    const positions = SECRET_ARGS[w];
    if (positions && toks[i + 1]?.kind === "punct" && toks[i + 1].text === "(") edits.push(...secretArgEdits(toks, i + 1, positions));
  }
  let out = applyEdits(sql, edits);
  out = out.replace(NAME_VALUE, (full: string, name: string, sep: string, value: string) => (value.includes(REDACTED) || /^(?:\$\d+|\?)$/.test(value) ? full : `${name}${sep}${maskValue(value)}`));
  out = out.replace(URI_CREDENTIALS, (_m, head: string) => `${head}:${REDACTED}@`);
  out = out.replace(AWS_KEY_ID, REDACTED);
  return out;
}

// ---------------------------------------------------------------------------------------------
// Literals out of a statement
// ---------------------------------------------------------------------------------------------

/** Replaces every string and numeric constant in a statement with `?`; identifiers and comments' structure stay. */
export function redactSqlLiterals(sql: string): string {
  let toks: Token[];
  try {
    toks = tokenize(sql);
  } catch {
    return "";
  }
  const edits: Edit[] = [];
  for (const t of toks) {
    if (t.kind === "string" || t.kind === "number") edits.push({ start: t.start, end: t.end, text: "?" });
    // Comments are free text typed by the user: they can hold values too.
    else if (t.kind === "comment") edits.push({ start: t.start, end: t.end, text: t.text.startsWith("--") ? "-- …" : "/* … */" });
  }
  return applyEdits(sql, edits);
}
