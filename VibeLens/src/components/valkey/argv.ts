// redis-cli-compatible command-line tokeniser: whitespace-separated, "double quotes" with C-style
// escapes (\n \r \t \b \a \xHH \\ \"), 'single quotes' (only \' is an escape), and backslash
// escaping of the next character outside quotes.

export type ArgvResult = { ok: true; argv: string[] } | { ok: false; error: string };

const HEX = /^[0-9a-fA-F]$/;
const isSpace = (c: string) => c === " " || c === "\t" || c === "\n" || c === "\r";

export function parseArgv(line: string): ArgvResult {
  const argv: string[] = [];
  let i = 0;
  const n = line.length;
  while (i < n) {
    while (i < n && isSpace(line[i])) i++;
    if (i >= n) break;
    let cur = "";
    const c = line[i];
    if (c === '"' || c === "'") {
      const quote = c;
      i++;
      let closed = false;
      while (i < n) {
        const ch = line[i];
        if (quote === '"' && ch === "\\") {
          const e = line[i + 1];
          if (e === undefined) break;
          if (e === "x" && HEX.test(line[i + 2] ?? "") && HEX.test(line[i + 3] ?? "")) {
            cur += String.fromCharCode(parseInt(line.slice(i + 2, i + 4), 16));
            i += 4;
            continue;
          }
          cur += e === "n" ? "\n" : e === "r" ? "\r" : e === "t" ? "\t" : e === "b" ? "\b" : e === "a" ? "\x07" : e;
          i += 2;
          continue;
        }
        if (quote === "'" && ch === "\\" && line[i + 1] === "'") {
          cur += "'";
          i += 2;
          continue;
        }
        if (ch === quote) {
          closed = true;
          i++;
          break;
        }
        cur += ch;
        i++;
      }
      if (!closed) return { ok: false, error: "Unbalanced quotes in command" };
      if (i < n && !isSpace(line[i])) return { ok: false, error: "A closing quote must be followed by a space" };
    } else {
      while (i < n && !isSpace(line[i])) {
        if (line[i] === "\\" && i + 1 < n) {
          cur += line[i + 1];
          i += 2;
        } else if (line[i] === '"' || line[i] === "'") {
          return { ok: false, error: "Unexpected quote inside an unquoted argument" };
        } else {
          cur += line[i];
          i++;
        }
      }
    }
    argv.push(cur);
  }
  return { ok: true, argv };
}

/** A binary argument: rendered as a double-quoted string with \xHH escapes for non-printable bytes. */
export interface BinaryArg {
  b64: string;
}
export type ArgvPart = string | BinaryArg;

const escapeText = (a: string) =>
  a
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .replace(/\r/g, "\\r")
    .replace(/\t/g, "\\t");

function escapeBinary(b64: string): string {
  let bin = "";
  try {
    bin = atob(b64);
  } catch {
    bin = "";
  }
  let out = "";
  for (let i = 0; i < bin.length; i++) {
    const c = bin.charCodeAt(i);
    if (c === 0x5c) out += "\\\\";
    else if (c === 0x22) out += '\\"';
    else if (c >= 0x20 && c < 0x7f) out += bin[i];
    else out += `\\x${c.toString(16).padStart(2, "0")}`;
  }
  return out;
}

/** Render argv back into a line that `parseArgv` round-trips (used by "Copy as command"). */
export function formatArgv(argv: readonly ArgvPart[]): string {
  return argv
    .map((a) => {
      if (typeof a !== "string") return `"${escapeBinary(a.b64)}"`;
      if (a !== "" && /^[A-Za-z0-9_.:*?$\-/@%+=,]+$/.test(a)) return a;
      return `"${escapeText(a)}"`;
    })
    .join(" ");
}
