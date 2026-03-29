export function esc(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

export function formatDuration(ms: number): string {
  if (ms <= 0) return "0ms";
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

export function formatTimestamp(unixMs: number | null, monotonicMs: number): string {
  return unixMs == null ? `${(monotonicMs / 1000).toFixed(3)}s` : new Date(unixMs).toLocaleString();
}

export function formatCollarId(id: number): string {
  return `0x${id.toString(16).toUpperCase().padStart(4, "0")}`;
}

export function fmtUs(us: number): string {
  const ms = Math.round(us / 1000);
  if (ms === 0) return "0ms";
  if (ms % 1000 === 0) return `${ms / 1000}s`;
  if (ms >= 1000) return `${(ms / 1000).toFixed(3)}s`;
  return `${ms}ms`;
}

export function cn(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(" ");
}
