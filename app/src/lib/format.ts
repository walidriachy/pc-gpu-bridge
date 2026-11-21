export function gb(bytes: number | null | undefined, digits = 1): string {
  if (bytes == null || !isFinite(bytes)) return "–";
  return `${(bytes / 1024 ** 3).toFixed(digits)} GB`;
}

export function duration(secs: number | null | undefined): string {
  if (secs == null || !isFinite(secs)) return "–";
  if (secs < 1) return "under a second";
  if (secs < 60) return `${Math.round(secs)} s`;
  const m = Math.floor(secs / 60);
  const s = Math.round(secs % 60);
  if (m < 60) return s ? `${m} min ${s} s` : `${m} min`;
  const h = Math.floor(m / 60);
  return `${h} h ${m % 60} min`;
}

export function timeAgo(unixSecs: number, now = Date.now() / 1000): string {
  const d = Math.max(0, now - unixSecs);
  if (d < 60) return "just now";
  if (d < 3600) return `${Math.floor(d / 60)} min ago`;
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`;
  return `${Math.floor(d / 86400)} days ago`;
}

/** Fraction (0–1) of memory in use. */
export function usedFraction(total: number | undefined, available: number | undefined): number {
  if (!total || available == null) return 0;
  return Math.min(1, Math.max(0, 1 - available / total));
}

export function speedup(pcSecs: number, macSecs: number): string {
  if (!pcSecs || !macSecs) return "";
  const x = macSecs / pcSecs;
  return x >= 1.2 ? `about ${x.toFixed(1)}× faster` : "about the same speed";
}

/** Keep only digits, max 6 — for the pairing code box. */
export function cleanCode(input: string): string {
  return input.replace(/\D/g, "").slice(0, 6);
}
