// A deliberately small markdown parser for streamed assistant text: paragraphs, headings,
// bullet / numbered lists, block quotes, fenced code, and inline code / bold / italic / links.
// Pure and total: any input yields blocks, an unterminated fence renders as an open code block.

export type Inline =
  | { t: "text"; v: string }
  | { t: "code"; v: string }
  | { t: "bold"; v: Inline[] }
  | { t: "italic"; v: Inline[] }
  | { t: "link"; href: string; v: Inline[] };

export type Block =
  | { t: "p"; v: Inline[] }
  | { t: "h"; level: 1 | 2 | 3; v: Inline[] }
  | { t: "ul"; items: Inline[][] }
  | { t: "ol"; start: number; items: Inline[][] }
  | { t: "quote"; v: Inline[] }
  | { t: "table"; head: Inline[][]; align: ("left" | "center" | "right" | null)[]; rows: Inline[][][] }
  | { t: "code"; lang: string; code: string; open: boolean };

const TABLE_RULE = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

/** Splits a table row on unescaped pipes outside backtick code. */
export function splitTableRow(line: string): string[] {
  const t = line.trim().replace(/^\|/, "").replace(/(?<!\\)\|$/, "");
  const cells: string[] = [];
  let buf = "";
  let inCode = false;
  for (let i = 0; i < t.length; i++) {
    const c = t[i];
    if (c === "\\" && t[i + 1] === "|") {
      buf += "|";
      i++;
    } else if (c === "`") {
      inCode = !inCode;
      buf += c;
    } else if (c === "|" && !inCode) {
      cells.push(buf.trim());
      buf = "";
    } else buf += c;
  }
  cells.push(buf.trim());
  return cells;
}

const FENCE = /^\s{0,3}(`{3,}|~{3,})\s*([\w+#.-]*)[^`]*$/;

export function parseBlocks(src: string): Block[] {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const out: Block[] = [];
  let i = 0;
  const startsBlock = (l: string) => FENCE.test(l) || /^\s{0,3}#{1,6}\s/.test(l) || /^\s*([-*+]|\d+[.)])\s+/.test(l) || /^\s{0,3}>\s?/.test(l);

  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "") {
      i++;
      continue;
    }
    const f = FENCE.exec(line);
    if (f) {
      const marker = f[1][0];
      const len = f[1].length;
      const body: string[] = [];
      i++;
      let closed = false;
      while (i < lines.length) {
        const m = new RegExp(`^\\s{0,3}${marker === "`" ? "`" : "~"}{${len},}\\s*$`).test(lines[i]);
        if (m) {
          closed = true;
          i++;
          break;
        }
        body.push(lines[i]);
        i++;
      }
      out.push({ t: "code", lang: (f[2] || "").toLowerCase(), code: body.join("\n"), open: !closed });
      continue;
    }
    const h = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (h) {
      out.push({ t: "h", level: Math.min(3, h[1].length) as 1 | 2 | 3, v: parseInline(h[2]) });
      i++;
      continue;
    }
    if (/^\s{0,3}>\s?/.test(line)) {
      const q: string[] = [];
      while (i < lines.length && /^\s{0,3}>\s?/.test(lines[i])) {
        q.push(lines[i].replace(/^\s{0,3}>\s?/, ""));
        i++;
      }
      out.push({ t: "quote", v: parseInline(q.join(" ")) });
      continue;
    }
    const ul = /^\s*[-*+]\s+(.*)$/.exec(line);
    const ol = /^\s*(\d+)[.)]\s+(.*)$/.exec(line);
    if (ul || ol) {
      const ordered = !!ol;
      const start = ol ? Number(ol[1]) : 1;
      const items: Inline[][] = [];
      while (i < lines.length) {
        const m = ordered ? /^\s*\d+[.)]\s+(.*)$/.exec(lines[i]) : /^\s*[-*+]\s+(.*)$/.exec(lines[i]);
        if (m) {
          let text = m[1];
          i++;
          // continuation lines (indented, not a new block)
          while (i < lines.length && lines[i].trim() !== "" && /^\s{2,}\S/.test(lines[i]) && !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
            text += " " + lines[i].trim();
            i++;
          }
          items.push(parseInline(text));
        } else if (lines[i].trim() === "" && i + 1 < lines.length && (ordered ? /^\s*\d+[.)]\s+/ : /^\s*[-*+]\s+/).test(lines[i + 1])) {
          i++; // loose list
        } else break;
      }
      out.push(ordered ? { t: "ol", start, items } : { t: "ul", items });
      continue;
    }
    if (line.includes("|") && i + 1 < lines.length && TABLE_RULE.test(lines[i + 1]) && lines[i + 1].includes("-")) {
      const head = splitTableRow(line);
      const align = splitTableRow(lines[i + 1]).map((c) => (c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : null)) as ("left" | "center" | "right" | null)[];
      if (head.length === align.length) {
        i += 2;
        const rows: Inline[][][] = [];
        while (i < lines.length && lines[i].trim() !== "" && lines[i].includes("|")) {
          const cells = splitTableRow(lines[i]);
          rows.push(head.map((_, k) => parseInline(cells[k] ?? "")));
          i++;
        }
        out.push({ t: "table", head: head.map((h) => parseInline(h)), align, rows });
        continue;
      }
    }
    const para: string[] = [line.trim()];
    i++;
    while (i < lines.length && lines[i].trim() !== "" && !startsBlock(lines[i])) {
      para.push(lines[i].trim());
      i++;
    }
    out.push({ t: "p", v: parseInline(para.join(" ")) });
  }
  return out;
}

const SAFE_HREF = /^https?:\/\/[^\s]+$/i;

export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  let buf = "";
  const flush = () => {
    if (buf) out.push({ t: "text", v: buf });
    buf = "";
  };
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === "\\" && i + 1 < src.length && /[\\`*_[\]()#>~-]/.test(src[i + 1])) {
      buf += src[i + 1];
      i += 2;
      continue;
    }
    if (c === "`") {
      let n = 1;
      while (src[i + n] === "`") n++;
      const fence = "`".repeat(n);
      const end = src.indexOf(fence, i + n);
      if (end > -1) {
        flush();
        out.push({ t: "code", v: src.slice(i + n, end).replace(/^ (.*) $/, "$1") });
        i = end + n;
        continue;
      }
    }
    if ((c === "*" || c === "_") && src[i + 1] === c) {
      const end = src.indexOf(c + c, i + 2);
      if (end > i + 2 && !/\s/.test(src[i + 2]) && (c === "*" || !/\w/.test(src[i - 1] ?? " "))) {
        flush();
        out.push({ t: "bold", v: parseInline(src.slice(i + 2, end)) });
        i = end + 2;
        continue;
      }
    }
    if ((c === "*" || c === "_") && src[i + 1] !== c && src[i + 1] !== " " && src[i + 1] !== undefined) {
      const end = src.indexOf(c, i + 1);
      const prevWord = c === "_" && /\w/.test(src[i - 1] ?? " ");
      if (end > i + 1 && !/\s/.test(src[end - 1]) && !prevWord && (c === "*" || !/\w/.test(src[end + 1] ?? " "))) {
        flush();
        out.push({ t: "italic", v: parseInline(src.slice(i + 1, end)) });
        i = end + 1;
        continue;
      }
    }
    if (c === "[") {
      const m = /^\[([^\]]+)\]\(([^)\s]+)\)/.exec(src.slice(i));
      if (m && SAFE_HREF.test(m[2])) {
        flush();
        out.push({ t: "link", href: m[2], v: parseInline(m[1]) });
        i += m[0].length;
        continue;
      }
    }
    buf += c;
    i++;
  }
  flush();
  return out;
}
