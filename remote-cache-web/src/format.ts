export function human(bytes: number | null | undefined): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes || 0;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(1)} ${units[unit]}`;
}

export function ago(at: string | null | undefined): string {
  if (!at) return "never";
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(at)) / 1000));
  if (Number.isNaN(seconds)) return "—";
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
  if (seconds < 172800) return `${Math.round(seconds / 3600)}h ago`;
  return `${Math.round(seconds / 86400)}d ago`;
}

export function rate(value: number | null | undefined): string {
  return value === null || value === undefined ? "—" : `${value.toFixed(0)}%`;
}

export function duration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${(ms / 1000).toFixed(1)}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(s / 3600)}h${String(Math.floor(s / 60) % 60).padStart(2, "0")}m`;
}

export function compact(n: number): string {
  return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(
    n,
  );
}

/** The last `count` UTC days, oldest first, as `YYYY-MM-DD`. */
export function lastDays(count: number): string[] {
  const out: string[] = [];
  const today = new Date();
  for (let i = count - 1; i >= 0; i--) {
    const d = new Date(
      Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate() - i),
    );
    out.push(d.toISOString().slice(0, 10));
  }
  return out;
}

export function shortDay(day: string): string {
  return new Date(`${day}T00:00:00Z`).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    timeZone: "UTC",
  });
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const YEAR = 365 * DAY;

/**
 * A long span of saved time, at the two largest units that matter: "2y 40d",
 * "3d 4h", "5h 12m", "42m", "18s". Saved time adds up across every runner, so
 * a busy cache reaches days quickly and, given long enough, years.
 */
export function saved(ms: number | null | undefined): string {
  const t = ms ?? 0;
  if (t < MINUTE) return `${Math.floor(t / 1000)}s`;
  if (t < HOUR) return `${Math.floor(t / MINUTE)}m`;
  if (t < DAY) return `${Math.floor(t / HOUR)}h ${Math.floor((t % HOUR) / MINUTE)}m`;
  if (t < YEAR) return `${Math.floor(t / DAY)}d ${Math.floor((t % DAY) / HOUR)}h`;
  return `${Math.floor(t / YEAR)}y ${Math.floor((t % YEAR) / DAY)}d`;
}

/**
 * The unit a chart of saved time should count in, picked from its largest
 * value so the axis reads "3y" rather than "26,280h".
 */
export function timeUnit(maxMs: number): { ms: number; suffix: string } {
  if (maxMs >= 2 * YEAR) return { ms: YEAR, suffix: "y" };
  if (maxMs >= 2 * DAY) return { ms: DAY, suffix: "d" };
  if (maxMs >= 2 * HOUR) return { ms: HOUR, suffix: "h" };
  return { ms: MINUTE, suffix: "m" };
}
