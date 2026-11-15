// Line-level diff for the inline AI preview: unchanged lines stay put, removed lines are marked in
// place, added lines appear as a block. Classic LCS; inputs are a statement or two, so O(n·m) is fine.

export type DiffOp = { type: "equal" | "del" | "add"; line: string };

export function diffLines(oldLines: string[], newLines: string[]): DiffOp[] {
  const n = oldLines.length;
  const m = newLines.length;
  // Trim common head/tail first: cheap and keeps the DP table tiny for the usual "add a WHERE" edit.
  let head = 0;
  while (head < n && head < m && oldLines[head] === newLines[head]) head++;
  let tail = 0;
  while (tail < n - head && tail < m - head && oldLines[n - 1 - tail] === newLines[m - 1 - tail]) tail++;
  const a = oldLines.slice(head, n - tail);
  const b = newLines.slice(head, m - tail);
  const out: DiffOp[] = oldLines.slice(0, head).map((line) => ({ type: "equal" as const, line }));

  const w = b.length + 1;
  if (a.length * b.length > 4_000_000) {
    // Pathologically large: show it as a wholesale replacement.
    for (const line of a) out.push({ type: "del", line });
    for (const line of b) out.push({ type: "add", line });
  } else {
    const dp = new Uint32Array((a.length + 1) * w);
    for (let i = a.length - 1; i >= 0; i--) {
      for (let j = b.length - 1; j >= 0; j--) {
        dp[i * w + j] = a[i] === b[j] ? dp[(i + 1) * w + j + 1] + 1 : Math.max(dp[(i + 1) * w + j], dp[i * w + j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    while (i < a.length && j < b.length) {
      if (a[i] === b[j]) {
        out.push({ type: "equal", line: a[i] });
        i++;
        j++;
      } else if (dp[(i + 1) * w + j] >= dp[i * w + j + 1]) {
        out.push({ type: "del", line: a[i++] });
      } else {
        out.push({ type: "add", line: b[j++] });
      }
    }
    while (i < a.length) out.push({ type: "del", line: a[i++] });
    while (j < b.length) out.push({ type: "add", line: b[j++] });
  }
  for (const line of oldLines.slice(n - tail)) out.push({ type: "equal", line });
  return out;
}

export interface DiffHunks {
  /** 0-based indexes into the old lines that are removed. */
  removed: number[];
  /** Blocks of new lines, each shown after old line `afterOld` (-1 = before the first line). */
  added: { afterOld: number; lines: string[] }[];
}

/** Collapses an op list into "which old lines vanish" and "which new blocks appear where". */
export function hunksOf(ops: DiffOp[]): DiffHunks {
  const removed: number[] = [];
  const added: { afterOld: number; lines: string[] }[] = [];
  let oldIdx = -1;
  let cur: { afterOld: number; lines: string[] } | null = null;
  for (const op of ops) {
    if (op.type === "add") {
      if (!cur) {
        cur = { afterOld: oldIdx, lines: [] };
        added.push(cur);
      }
      cur.lines.push(op.line);
    } else {
      cur = null;
      oldIdx++;
      if (op.type === "del") removed.push(oldIdx);
    }
  }
  return { removed, added };
}
