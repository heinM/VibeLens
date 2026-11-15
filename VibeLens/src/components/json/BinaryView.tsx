// Hex / Base64 / text view for binary payloads ({$bytes, len} cells and Valkey binary strings).
import { useMemo, useState } from "react";
import { Copy } from "lucide-react";
import { Segmented } from "../ui/wsKit";
import { copyToClipboard } from "../ui/clipboard";
import { IconButton } from "../ui";

const HEX_LIMIT = 4096;

function decodeBase64(b64: string): Uint8Array {
  try {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return new Uint8Array();
  }
}

export function hexDump(bytes: Uint8Array, limit = HEX_LIMIT): string {
  const lines: string[] = [];
  const n = Math.min(bytes.length, limit);
  for (let off = 0; off < n; off += 16) {
    const chunk = bytes.subarray(off, Math.min(off + 16, n));
    const hex = Array.from(chunk, (b) => b.toString(16).padStart(2, "0")).join(" ");
    const ascii = Array.from(chunk, (b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : ".")).join("");
    lines.push(`${off.toString(16).padStart(8, "0")}  ${hex.padEnd(47, " ")}  ${ascii}`);
  }
  if (bytes.length > n) lines.push(`… ${bytes.length - n} more bytes`);
  return lines.join("\n");
}

type Mode = "hex" | "base64" | "text";

export function BinaryView({ base64, len }: { base64: string; len: number }) {
  const [mode, setMode] = useState<Mode>("hex");
  const bytes = useMemo(() => decodeBase64(base64), [base64]);
  const text = useMemo(() => {
    if (mode === "hex") return hexDump(bytes);
    if (mode === "base64") return base64;
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes.subarray(0, 64 * 1024));
  }, [mode, bytes, base64]);

  return (
    <div className="flex h-full min-h-0 flex-col gap-2">
      <div className="flex items-center gap-2">
        <Segmented
          label="Binary encoding"
          value={mode}
          onChange={setMode}
          options={[
            { value: "hex", label: "Hex" },
            { value: "base64", label: "Base64" },
            { value: "text", label: "Text" },
          ]}
        />
        <span className="text-xs text-muted">{len.toLocaleString("en-US")} bytes</span>
        <IconButton label="Copy as base64" className="ml-auto" onClick={() => void copyToClipboard(base64)}>
          <Copy className="size-3.5" />
        </IconButton>
      </div>
      <pre className="selectable min-h-0 flex-1 overflow-auto rounded-md border border-border bg-bg p-2 font-mono text-[12px] leading-5 text-fg">{text}</pre>
    </div>
  );
}
