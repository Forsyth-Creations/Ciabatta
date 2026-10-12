/**
 * The remote cache's JSON API, as this app uses it.
 *
 * The session token lives in this module's memory and nowhere else. Putting it
 * in browser storage would leave a credential behind on a shared machine long
 * after the tab was closed — the price is signing in again after a reload.
 */

let token: string | null = null;

export function setToken(value: string | null) {
  token = value;
}

export function hasToken() {
  return token !== null;
}

export async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (token) headers["Authorization"] = "Bearer " + token;
  const response = await fetch(path, { ...options, headers });
  const text = await response.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    /* not JSON */
  }
  if (!response.ok) {
    const message = (body as { error?: string } | null)?.error;
    throw new Error(message || text || response.statusText);
  }
  return body as T;
}

export async function login(username: string, password: string) {
  const body = await api<{ token: string }>("/api/auth/login", {
    method: "POST",
    body: JSON.stringify({ username, password }),
  });
  token = body.token;
}

// ─── Shapes ─────────────────────────────────────────────────────────────────

export interface Health {
  version: string;
  auth: string;
  release?: { version?: string } | null;
}

export interface Counters {
  hits: number;
  misses: number;
  uploads: number;
  bytes_served: number;
  bytes_stored: number;
  /** Build time hits spared their clients. Absent from older servers. */
  saved_ms?: number;
}

export interface Day {
  hits: number;
  misses: number;
  uploads: number;
  bytes_served: number;
  bytes_stored: number;
  saved_ms?: number;
}

export interface Target {
  name: string;
  hits: number;
  misses: number;
  uploads: number;
  saved_ms?: number;
  last_hit_at: string | null;
  last_miss_at: string | null;
  last_upload_at: string | null;
}

export interface ProjectStats {
  project: { id: string; name: string };
  counters: Counters;
  hit_rate: number | null;
  entries: number;
  bytes: number;
  last_used_at: string | null;
  last_saved_at: string | null;
  newest_entry_at: string | null;
  targets: Target[];
  stale_workflows: number;
}

export interface Event {
  at: string;
  project: string;
  project_name: string;
  kind: "hit" | "miss" | "upload" | "touch";
  target: string | null;
  user: string | null;
  bytes: number;
  count: number;
}

export interface Insight {
  severity: "warn" | "info";
  project?: string;
  target?: string;
  message: string;
  action: string;
}

export interface EntrySummary {
  project: string;
  target: string;
  size: number;
  created_at: string;
  last_used_at: string;
}

export interface Stats {
  storage: {
    entries: number;
    bytes: number;
    human: string;
    max_bytes: number | null;
    percent: number | null;
  };
  counters: Counters;
  hit_rate: number | null;
  activity: {
    since: string | null;
    last_used_at: string | null;
    last_hit_at: string | null;
    last_miss_at: string | null;
    last_saved_at: string | null;
    last_eviction: { at: string; removed: number; freed: number } | null;
    /** Older servers don't send this; the charts say so rather than break. */
    daily?: Record<string, Day>;
    recent: Event[];
  };
  insights: Insight[];
  largest: EntrySummary[];
  least_recently_used: EntrySummary[];
  retention: { description: string };
  sessions: number;
  tls: boolean;
  projects: ProjectStats[];
}

export type Outcome = "success" | "failed" | "stopped" | "skipped" | string;

export interface BuildSummary {
  id: string;
  project: string;
  project_name?: string;
  workflow: string;
  outcome: Outcome;
  started_at: string;
  finished_at: string;
  duration_ms: number | null;
  branch: string | null;
  commit: string | null;
  tag: string | null;
  user: string | null;
  host: string | null;
  ci: string | null;
  reported_by: string | null;
  step_count: number;
  /** Steps restored from a cache rather than run. */
  reused: number;
  /** Steps that ran. */
  built: number;
  failed: number;
}

export interface Step {
  name: string;
  workspace: string | null;
  kind: string | null;
  status: string;
  cache: string | null;
  cache_source: string | null;
  cache_key: string | null;
  duration_ms: number | null;
  note: string | null;
  needs: string[];
}

export interface Build extends BuildSummary {
  build_number: string | null;
  version: string;
  error: string | null;
  steps: Step[];
}

export interface Release {
  project: string;
  project_name?: string;
  name: string;
  version: string;
  created_at: string;
  commit: string | null;
  files: number;
  size: number;
}

export interface User {
  name: string;
  admin: boolean;
  read_only: boolean;
  from_config: boolean;
  created_at: string | null;
}

export interface Users {
  users: User[];
  mode: string;
  locked_out?: boolean;
}

export interface CreatedUser {
  user: { name: string };
  token: string;
  note: string;
  login: string;
}
