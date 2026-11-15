// A small iceberg mark: the tip above the waterline, the bulk below. Used where lucide has no fitting icon.
import { cn } from "../../lib/cn";

export function IcebergGlyph({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden className={cn("size-4", className)} fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" strokeLinecap="round">
      <path d="M6.2 6.6 8.4 2.2 11 6.6" />
      <path d="M1.5 7.4h13" />
      <path d="M3.4 7.4 6 13.6h4.4l2.2-6.2" fill="currentColor" fillOpacity="0.22" />
      <path d="M6.9 9.4 8.2 11.6" />
    </svg>
  );
}
