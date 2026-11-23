export function gb(bytes: number | null | undefined, digits = 1): string {
  if (bytes == null || !isFinite(bytes)) return "–";
  return `${(bytes / 1024 ** 3).toFixed(digits)} GB`;
}

export function duration(secs: number | null | undefined): string {
  if (secs == null || !isFinite(secs)) return "–";
  if (secs < 1) return "under 1 s";
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

/** Free space (GB) a program needs before installing: its size plus 2 GB of room, like the helper checks. */
export function spaceNeededGb(sizeGb: number): number {
  return Math.ceil(sizeGb + 2);
}

/** True when there's room to install; an unknown free space (negative) never blocks. */
export function hasRoom(freeGb: number, sizeGb: number): boolean {
  return freeGb < 0 || freeGb >= sizeGb + 2;
}

/** "about 300 MB" / "about 2.5 GB" */
export function sizeText(sizeGb: number): string {
  if (sizeGb < 1) return `about ${Math.max(10, Math.round((sizeGb * 1000) / 10) * 10)} MB`;
  return `about ${Number.isInteger(sizeGb) ? sizeGb : sizeGb.toFixed(1)} GB`;
}

/** "C:\\" -> "C:" */
export function driveName(drive: string): string {
  return drive.replace(/[\\/]+$/, "") || drive || "C:";
}

/**
 * A job's name as people read it: the command without the setup in front of it.
 * `$env:A="x"; $env:B='y'; node render.mjs` -> `node render.mjs`, `set A=x && node …` -> `node …`.
 */
export function jobLabel(name: string | null | undefined): string {
  let s = (name || "").trim();
  const lead = /^(?:\$(?:env:)?[\w.]+\s*=\s*(?:"[^"]*"|'[^']*'|[^;\s]+)\s*;\s*|&\s+|set\s+"?[\w.]+=[^&]*&&\s*|[A-Za-z_]\w*=(?:"[^"]*"|'[^']*'|\S+)\s+)/i;
  for (let i = 0; i < 20 && lead.test(s); i++) s = s.replace(lead, "").trim();
  return s || (name || "").trim() || "job";
}
